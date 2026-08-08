//! Attempts run on Tokio at the same time: the engine hands their outcomes to the driver as they
//! end, runs a task again after a failure of the machine or when the driver asks, and stops
//! the attempts in flight when the run is cancelled.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use workflow_graph::{
    Attempt, CancellationToken, Driver, DriverError, Engine, Event, ExecuteError, Executor,
    Language, Observer, Order, Outcome, RunOptions, Running, Task, TaskId,
};

/// Asks for fixed tasks at the start, notes the outcomes it hears, and asks again for a task
/// whose exit code is not 0 until it has heard it `retries` times.
#[derive(Default)]
struct Tasks {
    first: Vec<Task>,
    retries: u32,
    heard: Vec<(TaskId, u32, i32)>,
}

impl Driver for Tasks {
    fn language(&self) -> Language {
        Language::Snakemake
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(std::mem::take(&mut self.first))
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.heard
            .push((outcome.id, outcome.attempt, outcome.exit_code));
        if outcome.exit_code != 0 && outcome.attempt <= self.retries {
            return Ok(vec![task(outcome.id.0, &[])]);
        }
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn task(id: u64, after: &[u64]) -> Task {
    Task {
        id: TaskId(id),
        name: format!("t{id}"),
        command: vec!["true".into()],
        after: after.iter().copied().map(TaskId).collect(),
        ..Task::default()
    }
}

/// Sleeps as long as each task says, in milliseconds by its id, and fails the attempts it is
/// told to: with the machine, or with an exit code.
#[derive(Default)]
struct Sleeper {
    sleep: HashMap<u64, u64>,
    machine_failures: HashMap<(u64, u32), ()>,
    exit_codes: HashMap<(u64, u32), i32>,
    /// Whether an attempt saw the run stop.
    cancelled: Arc<Mutex<Vec<TaskId>>>,
}

impl Executor for Sleeper {
    fn run(&mut self, attempt: Attempt) -> Running {
        let id = attempt.task.id;
        let key = (id.0, attempt.number);
        let sleep = Duration::from_millis(self.sleep.get(&id.0).copied().unwrap_or(0));
        let machine = self.machine_failures.contains_key(&key);
        let exit_code = self.exit_codes.get(&key).copied().unwrap_or(0);
        let cancelled = self.cancelled.clone();
        Running::new(async move {
            tokio::select! {
                () = tokio::time::sleep(sleep) => {}
                () = attempt.cancel.cancelled() => {
                    cancelled.lock().unwrap().push(id);
                    return Err(ExecuteError::Cancelled);
                }
            }
            if machine {
                return Err(ExecuteError::Machine {
                    task: attempt.task.name.clone(),
                    message: "the container engine went away".into(),
                });
            }
            Ok(Outcome {
                id,
                exit_code,
                ..Outcome::default()
            })
        })
    }
}

/// Notes the attempts that ended and whether the engine ran them again.
#[derive(Default)]
struct Retries(Vec<(TaskId, u32, bool)>);

impl Observer for Retries {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Finished {
            outcome,
            attempt,
            retried,
        } = event
        {
            self.0.push((outcome.id, attempt, retried));
        }
    }
}

fn options(max_pending: usize, order: Order) -> RunOptions {
    RunOptions { max_pending, order }
}

#[test]
fn attempts_run_at_the_same_time_and_end_in_their_order() {
    let mut driver = Tasks {
        first: (1..=4).map(|id| task(id, &[])).collect(),
        ..Tasks::default()
    };
    let mut executor = Sleeper {
        sleep: HashMap::from([(1, 400), (2, 300), (3, 200), (4, 100)]),
        ..Sleeper::default()
    };
    let begin = Instant::now();
    let execution = Engine::new(&mut executor)
        .options(options(4, Order::Finished))
        .run("sleepers", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    // Four attempts of up to 400 ms that run at the same time take less than their sum.
    assert!(
        begin.elapsed() < Duration::from_millis(900),
        "{:?}",
        begin.elapsed()
    );
    let heard: Vec<u64> = driver.heard.iter().map(|(id, _, _)| id.0).collect();
    assert_eq!(heard, [4, 3, 2, 1]);
}

#[test]
fn deterministic_orders_do_not_depend_on_when_attempts_end() {
    let mut driver = Tasks {
        first: (1..=3).map(|id| task(id, &[])).collect(),
        ..Tasks::default()
    };
    let mut executor = Sleeper {
        sleep: HashMap::from([(1, 150), (2, 0), (3, 50)]),
        ..Sleeper::default()
    };
    Engine::new(&mut executor)
        .options(options(3, Order::AsAsked))
        .run("sleepers", &mut driver);
    let heard: Vec<u64> = driver.heard.iter().map(|(id, _, _)| id.0).collect();
    assert_eq!(heard, [1, 2, 3]);
}

#[test]
fn the_engine_runs_a_task_again_after_the_machine_failed() {
    let mut driver = Tasks {
        first: vec![task(1, &[])],
        ..Tasks::default()
    };
    let mut executor = Sleeper {
        machine_failures: HashMap::from([((1, 1), ())]),
        ..Sleeper::default()
    };
    let mut retries = Retries::default();
    let execution = Engine::new(&mut executor)
        .observer(&mut retries)
        .run("machine", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(driver.heard, [(TaskId(1), 2, 0)]);
    assert_eq!(retries.0, [(TaskId(1), 1, true), (TaskId(1), 2, false)]);
    // Without retries left, the failure of the machine stops the run.
    let mut driver = Tasks {
        first: vec![task(1, &[])],
        ..Tasks::default()
    };
    let mut executor = Sleeper {
        machine_failures: HashMap::from([((1, 1), ()), ((1, 2), ())]),
        ..Sleeper::default()
    };
    let execution = Engine::new(&mut executor)
        .retries(1)
        .run("machine", &mut driver);
    assert!(
        matches!(execution.error, Some(ExecuteError::Machine { .. })),
        "{:?}",
        execution.error
    );
}

#[test]
fn a_driver_retries_a_failed_task_and_the_tasks_after_it_wait() {
    let mut driver = Tasks {
        first: vec![task(1, &[]), task(2, &[1])],
        retries: 1,
        ..Tasks::default()
    };
    let mut executor = Sleeper {
        exit_codes: HashMap::from([((1, 1), 137)]),
        ..Sleeper::default()
    };
    let execution = Engine::new(&mut executor)
        .options(options(4, Order::Finished))
        .run("retries", &mut driver);
    let graph = execution.into_result().expect("the run succeeds");
    assert_eq!(
        driver.heard,
        [(TaskId(1), 1, 137), (TaskId(1), 2, 0), (TaskId(2), 1, 0)]
    );
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.nodes[0].exit_code, Some(0));
}

#[test]
fn a_cancelled_run_stops_its_attempts() {
    let mut driver = Tasks {
        first: vec![task(1, &[]), task(2, &[])],
        ..Tasks::default()
    };
    let mut executor = Sleeper {
        sleep: HashMap::from([(1, 60_000), (2, 60_000)]),
        ..Sleeper::default()
    };
    let cancelled = executor.cancelled.clone();
    let token = CancellationToken::new();
    let stop = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        stop.cancel();
    });
    let begin = Instant::now();
    let execution = Engine::new(&mut executor)
        .options(options(2, Order::Finished))
        .cancel(token)
        .run("cancelled", &mut driver);
    assert!(matches!(execution.error, Some(ExecuteError::Cancelled)));
    assert!(begin.elapsed() < Duration::from_secs(10));
    let mut stopped = cancelled.lock().unwrap().clone();
    stopped.sort();
    assert_eq!(stopped, [TaskId(1), TaskId(2)]);
}

/// Asks for fixed tasks at the start, notes the outcomes it hears, and drains the run after
/// the first failure, as Snakemake stops after a failed job.
#[derive(Default)]
struct Draining {
    first: Vec<Task>,
    heard: Vec<TaskId>,
    failed: bool,
}

impl Driver for Draining {
    fn language(&self) -> Language {
        Language::Snakemake
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(std::mem::take(&mut self.first))
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.heard.push(outcome.id);
        self.failed |= outcome.exit_code != 0;
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        match self.failed {
            true => Err(DriverError::new("a task failed")),
            false => Ok(()),
        }
    }

    fn draining(&self) -> bool {
        self.failed
    }
}

/// Runs a task for as long as it says, and holds the tasks that wait for their turn: task 3
/// waits until it is held, as a task that waits for its share of the machine.
#[derive(Default)]
struct Turns {
    held: CancellationToken,
    holds: usize,
}

impl Executor for Turns {
    fn run(&mut self, attempt: Attempt) -> Running {
        let id = attempt.task.id;
        let held = self.held.clone();
        Running::new(async move {
            match id.0 {
                1 => tokio::time::sleep(Duration::from_millis(10)).await,
                2 => tokio::time::sleep(Duration::from_millis(200)).await,
                _ => {
                    held.cancelled().await;
                    return Err(ExecuteError::Cancelled);
                }
            }
            let exit_code = if id.0 == 1 { 1 } else { 0 };
            Ok(Outcome {
                id,
                exit_code,
                ..Outcome::default()
            })
        })
    }

    fn hold(&mut self) {
        self.holds += 1;
        self.held.cancel();
    }
}

#[test]
fn a_draining_run_lets_running_tasks_end_and_starts_no_other() {
    // Task 1 fails while task 2 runs and task 3 waits for its turn; task 4 waits in the queue,
    // since at most three tasks are pending.
    let mut driver = Draining {
        first: vec![task(1, &[]), task(2, &[]), task(3, &[]), task(4, &[])],
        ..Draining::default()
    };
    let mut executor = Turns::default();
    let mut finished = Retries::default();
    let execution = Engine::new(&mut executor)
        .options(options(3, Order::Finished))
        .observer(&mut finished)
        .run("drain", &mut driver);
    let error = execution.error.expect("the run fails").to_string();
    assert_eq!(error, "a task failed");
    // The driver hears the failed task and the one that ran; task 3 never ran and task 4
    // never started.
    assert_eq!(driver.heard, [TaskId(1), TaskId(2)]);
    assert_eq!(executor.holds, 1);
    let ended: Vec<TaskId> = finished.0.iter().map(|(id, _, _)| *id).collect();
    assert_eq!(ended, [TaskId(1), TaskId(3), TaskId(2)]);
}
