//! The engine tells an observer what happens in a run, each step before its effect, and always
//! the end.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{
    Driver, DriverError, Engine, Event, ExecuteError, Executor, Language, Observer, Outcome, Task,
    TaskId,
};

/// Asks for task 1, then for task 2 after task 1, which reads a file of task 1.
struct Chain {
    fail_at_finish: bool,
}

impl Driver for Chain {
    fn language(&self) -> Language {
        Language::Cwl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(vec![task(1, &[])])
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        Ok(match outcome.id {
            TaskId(1) => vec![task(2, &[1])],
            _ => Vec::new(),
        })
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        match self.fail_at_finish {
            true => Err(DriverError::new("waits for input")),
            false => Ok(()),
        }
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

struct Succeed;

impl Executor for Succeed {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        Ok(Outcome {
            id: task.id,
            ..Outcome::default()
        })
    }
}

/// Notes each event in a line.
#[derive(Default)]
struct Notes(Vec<String>);

impl Observer for Notes {
    fn event(&mut self, event: Event<'_>) {
        self.0.push(match event {
            Event::Requested { task, edges } => format!("requested {} {}", task.id, edges.len()),
            Event::Started { task, attempt } => format!("started {task} {attempt}"),
            Event::Finished {
                outcome, retried, ..
            } => format!("finished {} {retried}", outcome.id),
            Event::Ended { error } => format!("ended {}", error.is_some()),
        });
    }
}

#[test]
fn observers_hear_each_step() {
    let mut notes = Notes::default();
    let mut executor = Succeed;
    let mut driver = Chain {
        fail_at_finish: false,
    };
    let execution = Engine::new(&mut executor)
        .observer(&mut notes)
        .run("chain", &mut driver);
    assert!(execution.error.is_none());
    let expected = [
        "requested 1 0",
        "started 1 1",
        "finished 1 false",
        "requested 2 1",
        "started 2 1",
        "finished 2 false",
        "ended false",
    ];
    assert_eq!(notes.0, expected);
}

#[test]
fn observers_hear_the_end_of_a_failed_run() {
    let mut notes = Notes::default();
    let mut driver = Chain {
        fail_at_finish: true,
    };
    let execution = Engine::new(&mut Succeed)
        .observer(&mut notes)
        .run("chain", &mut driver);
    assert!(execution.error.is_some());
    assert_eq!(notes.0.last().map(String::as_str), Some("ended true"));
}
