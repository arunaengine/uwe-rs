//! The engine tells an observer what happens in a run, each step before its effect, and always
//! the end.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{
    Attempt, Delivery, DeliveryMode, Driver, DriverError, Engine, Event, ExecuteError, Executor,
    Files, Language, LocalFiles, Observer, Outcome, Running, Task, TaskId,
};

/// Asks for task 1, then for task 2 after task 1, which reads a file of task 1. After task 1
/// it asks to deliver `delivered`, when given.
#[derive(Default)]
struct Chain {
    fail_at_finish: bool,
    delivered: Option<std::path::PathBuf>,
    asked: Vec<Delivery>,
    acknowledged: Vec<Delivery>,
    fail_at_delivery: bool,
    /// Whether the engine gave the files of the tasks.
    files: bool,
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
            TaskId(1) => {
                if let Some(source) = &self.delivered {
                    self.asked.push(Delivery {
                        location: source.to_string_lossy().into_owned(),
                        target: source
                            .with_extension("delivered")
                            .to_string_lossy()
                            .into_owned(),
                        mode: DeliveryMode::Copy,
                    });
                }
                vec![task(2, &[1])]
            }
            _ => Vec::new(),
        })
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.asked)
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        assert_eq!(
            std::fs::read(&delivery.target).unwrap(),
            std::fs::read(&delivery.location).unwrap()
        );
        if self.fail_at_delivery {
            return Err(DriverError::new("cannot record delivered output"));
        }
        self.acknowledged.push(delivery.clone());
        Ok(())
    }

    fn use_files(&mut self, files: std::sync::Arc<dyn Files>) {
        self.files = files.local("/any").is_ok();
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
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(self.execute(&attempt.task))
    }
}

impl Succeed {
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
            Event::Marked { task, marker } => format!("marked {task} {}", marker.kind.name()),
            Event::Delivered { delivery } => format!("delivered {}", delivery.location),
            Event::Ended { error } => format!("ended {}", error.is_some()),
        });
    }
}

#[test]
fn observers_hear_each_step() {
    let mut notes = Notes::default();
    let mut executor = Succeed;
    let mut driver = Chain::default();
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
        ..Chain::default()
    };
    let execution = Engine::new(&mut Succeed)
        .observer(&mut notes)
        .run("chain", &mut driver);
    assert!(execution.error.is_some());
    assert_eq!(notes.0.last().map(String::as_str), Some("ended true"));
}

/// Succeeds at once, with the files of this machine.
struct Real;

impl Executor for Real {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            ..Outcome::default()
        }))
    }

    fn files(&self) -> Option<std::sync::Arc<dyn Files>> {
        Some(std::sync::Arc::new(LocalFiles))
    }
}

#[test]
fn real_executors_deliver_what_drivers_ask_for() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let made = folder.path().join("out.txt");
    std::fs::write(&made, "made\n").expect("writes");
    let chain = || Chain {
        delivered: Some(made.clone()),
        ..Chain::default()
    };
    let mut notes = Notes::default();
    let mut simulated = chain();
    let execution = Engine::new(&mut Succeed)
        .observer(&mut notes)
        .run("chain", &mut simulated);
    assert!(execution.error.is_none());
    assert!(!simulated.files, "an executor without files gives none");
    let delivered = format!("delivered {}", made.display());
    assert_eq!(
        notes.0[4], delivered,
        "after the tasks that task 1 made the driver ask for"
    );
    // The simulator delivers nothing; a real executor does.
    let target = made.with_extension("delivered");
    assert!(!target.exists());
    assert!(simulated.acknowledged.is_empty());
    let mut real = chain();
    let execution = Engine::new(&mut Real).run("chain", &mut real);
    assert!(execution.error.is_none());
    assert!(real.files, "the driver gets the files of a real executor");
    assert_eq!(std::fs::read_to_string(target).expect("reads"), "made\n");
    assert_eq!(real.acknowledged.len(), 1);
}

#[test]
fn failed_copies_are_not_acknowledged_and_acknowledgement_errors_propagate() {
    let folder = tempfile::tempdir().unwrap();
    let made = folder.path().join("out.txt");
    let mut driver = Chain {
        delivered: Some(made.clone()),
        ..Chain::default()
    };
    let failed = Engine::new(&mut Real).run("chain", &mut driver);
    assert!(matches!(failed.error, Some(ExecuteError::Delivery { .. })));
    assert!(driver.acknowledged.is_empty());
    std::fs::write(&made, "made\n").unwrap();
    driver.fail_at_delivery = true;
    let failed = Engine::new(&mut Real).run("chain", &mut driver);
    assert!(
        matches!(failed.error, Some(ExecuteError::Driver(error)) if error.message == "cannot record delivered output")
    );
    assert!(driver.acknowledged.is_empty());
}
