//! Checks driver timer wakeups, individual cancellation and executor error routing.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Instant;

use workflow_graph::{
    Attempt, Driver, DriverError, Engine, ExecuteError, Executor, Language, Outcome, Running, Task,
    TaskError, TaskId,
};

struct Timed {
    deadline: Option<Instant>,
    cancel: bool,
    outcomes: Vec<Outcome>,
}

impl Driver for Timed {
    fn language(&self) -> Language {
        Language::Argo
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        if self.cancel {
            Ok(vec![Task {
                id: TaskId(1),
                ..Task::default()
            }])
        } else {
            Ok(Vec::new())
        }
    }

    fn wake_at(&self) -> Option<Instant> {
        self.deadline
    }

    fn wake(&mut self) -> Result<Vec<Task>, DriverError> {
        self.deadline = None;
        if self.cancel {
            Ok(Vec::new())
        } else {
            Ok(vec![Task {
                id: TaskId(1),
                ..Task::default()
            }])
        }
    }

    fn cancellations(&mut self) -> Vec<TaskId> {
        if self.cancel && self.deadline.is_none() {
            self.cancel = false;
            vec![TaskId(1)]
        } else {
            Vec::new()
        }
    }

    fn handles_error(&self, _task: TaskId, _error: &ExecuteError) -> bool {
        true
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.outcomes.push(outcome);
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

struct Machine;

impl Executor for Machine {
    fn run(&mut self, _attempt: Attempt) -> Running {
        Running::ended(Err(ExecuteError::Machine {
            task: "one".into(),
            message: "connection lost".into(),
        }))
    }
}

#[test]
fn wakes_empty_run() {
    let mut driver = Timed {
        deadline: Some(Instant::now()),
        cancel: false,
        outcomes: Vec::new(),
    };
    let result = Engine::new(&mut Machine).run("timed", &mut driver);
    assert!(result.error.is_none());
    assert_eq!(driver.outcomes.len(), 1);
    assert_eq!(driver.outcomes[0].error, Some(TaskError::Transient));
    assert_eq!(driver.outcomes[0].attempt, 1);
}

#[test]
fn cancels_queued_task() {
    let mut driver = Timed {
        deadline: Some(Instant::now()),
        cancel: true,
        outcomes: Vec::new(),
    };
    let result = Engine::new(&mut Machine).run("timed", &mut driver);
    assert!(result.error.is_none());
    assert_eq!(driver.outcomes[0].error, Some(TaskError::Cancelled));
}
