//! Checks the child driver adapter: task ids across retries, acceptance, starts, deliveries,
//! inputs and a single finish.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use wir_runtime::{Block, Child};
use workflow_graph::{
    Delivery, DeliveryMode, Driver, DriverError, Inputs, Language, Marker, MarkerKind, Note,
    Outcome, Task, TaskId,
};

/// What the scripted child saw.
#[derive(Default)]
struct Seen {
    finishes: u32,
    acknowledged: Vec<Delivery>,
    used: Option<Json>,
    started: Vec<(TaskId, u32)>,
}

/// A child that asks for tasks 10 and 11 (after 10), retries 10 once after a failure, and
/// accepts every attempt that exits with 0.
struct Script {
    seen: Rc<RefCell<Seen>>,
    accepted: HashSet<(TaskId, u32)>,
    inputs: Option<Inputs>,
    /// Fail the call that hears of task 11.
    broken: bool,
    deliveries: Vec<Delivery>,
}

impl Script {
    fn new(seen: &Rc<RefCell<Seen>>) -> Script {
        Script {
            seen: seen.clone(),
            accepted: HashSet::new(),
            inputs: None,
            broken: false,
            deliveries: Vec::new(),
        }
    }
}

fn task(id: u64, after: &[u64]) -> Task {
    Task {
        id: TaskId(id),
        name: format!("t{id}"),
        step: "t".into(),
        after: after.iter().map(|id| TaskId(*id)).collect(),
        ..Task::default()
    }
}

fn outcome(id: u64, attempt: u32, exit_code: i32) -> Outcome {
    Outcome {
        id: TaskId(id),
        attempt,
        exit_code,
        ..Outcome::default()
    }
}

fn delivery(location: &str) -> Delivery {
    Delivery {
        location: location.into(),
        target: format!("/out/{location}"),
        mode: DeliveryMode::Copy,
        overwrite: true,
    }
}

impl Driver for Script {
    fn language(&self) -> Language {
        Language::Cwl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(vec![task(10, &[]), task(11, &[10])])
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        if outcome.exit_code == 0 {
            self.accepted.insert((outcome.id, outcome.attempt));
        }
        if self.broken && outcome.id == TaskId(11) {
            return Err(DriverError::new("planning failed"));
        }
        match (outcome.id.0, outcome.exit_code) {
            (10, 1) => Ok(vec![task(10, &[])]),
            _ => Ok(Vec::new()),
        }
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.accepted.remove(&(task, attempt))
    }

    fn started(&mut self, task: TaskId, attempt: u32) {
        self.seen.borrow_mut().started.push((task, attempt));
    }

    fn cancellations(&mut self) -> Vec<TaskId> {
        vec![TaskId(11), TaskId(99)]
    }

    fn notes(&mut self) -> Vec<Note> {
        let marker = Marker::new(MarkerKind::Content, "read");
        vec![Note::Mark(TaskId(11), marker), Note::Read("/a".into())]
    }

    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        Ok(self.inputs.clone())
    }

    fn use_inputs(&mut self, values: Json) -> Result<(), DriverError> {
        self.seen.borrow_mut().used = Some(values);
        Ok(())
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        self.seen.borrow_mut().acknowledged.push(delivery.clone());
        Ok(())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        self.seen.borrow_mut().finishes += 1;
        Ok(())
    }
}

impl Block for Script {
    fn exported(&self) -> Result<Option<Json>, DriverError> {
        Ok(Some(json!({"out": 1})))
    }
}

/// A counter of parent ids from 100.
fn counter() -> impl FnMut() -> TaskId {
    let mut next = 99;
    move || {
        next += 1;
        TaskId(next)
    }
}

#[test]
fn retries_keep_ids() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut child = Child::new(Box::new(Script::new(&seen)));
    let mut next = counter();
    let first = child.start(&mut next).unwrap();
    let ids: Vec<u64> = first.iter().map(|task| task.id.0).collect();
    assert_eq!(ids, [100, 101]);
    assert_eq!(first[1].after, [TaskId(100)], "after names the parent id");
    let retry = child.complete(outcome(100, 1, 1), &mut next).unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(
        retry[0].id,
        TaskId(100),
        "a retried task keeps its parent id"
    );
    assert!(!child.take_acceptance(TaskId(100), 1));
    child.started(TaskId(100), 2);
    child.started(TaskId(7), 1);
    assert_eq!(
        seen.borrow().started,
        [(TaskId(10), 2)],
        "a start reaches the child by its own id"
    );
    assert_eq!(
        child.cancellations(),
        [TaskId(101)],
        "unknown ids are dropped"
    );
    let notes = child.notes();
    assert!(matches!(&notes[0], Note::Mark(TaskId(101), _)), "{notes:?}");
    assert!(matches!(&notes[1], Note::Read(_)), "{notes:?}");
    assert!(
        child
            .complete(outcome(100, 2, 0), &mut next)
            .unwrap()
            .is_empty()
    );
    assert!(child.take_acceptance(TaskId(100), 2));
    assert!(
        !child.take_acceptance(TaskId(100), 2),
        "acceptance is taken once"
    );
    assert!(child.busy(), "task 101 has not ended");
    child.complete(outcome(101, 1, 0), &mut next).unwrap();
    assert!(!child.busy());
    assert_eq!(child.finish().unwrap(), Some(json!({"out": 1})));
    assert!(child.finish().is_err(), "a child finishes once");
    assert_eq!(seen.borrow().finishes, 1);
    assert!(child.owns(TaskId(101)) && !child.owns(TaskId(102)));
}

#[test]
fn stops_children() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut child = Child::new(Box::new(Script::new(&seen)));
    let mut next = counter();
    child.start(&mut next).unwrap();
    assert_eq!(
        child.stop(),
        [TaskId(100), TaskId(101)],
        "the tasks that run stop"
    );
    assert!(
        child.finished() && !child.busy(),
        "a stopped child takes no more calls"
    );
    assert!(child.stop().is_empty());
    assert_eq!(
        seen.borrow().finishes,
        0,
        "a stopped child never finishes with results"
    );
}

#[test]
fn errors_keep_acceptance() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut script = Script::new(&seen);
    script.broken = true;
    let mut child = Child::new(Box::new(script));
    let mut next = counter();
    child.start(&mut next).unwrap();
    assert!(child.complete(outcome(101, 1, 0), &mut next).is_err());
    assert!(child.take_acceptance(TaskId(101), 1));
    assert!(
        child.complete(outcome(7, 1, 0), &mut next).is_err(),
        "not a task of the child"
    );
}

#[test]
fn routes_deliveries() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut first = Script::new(&seen);
    first.deliveries = vec![delivery("a")];
    let other = Rc::new(RefCell::new(Seen::default()));
    let mut second = Script::new(&other);
    second.deliveries = vec![delivery("b")];
    let (mut first, mut second) = (Child::new(Box::new(first)), Child::new(Box::new(second)));
    assert_eq!(first.deliveries(), [delivery("a")]);
    assert_eq!(second.deliveries(), [delivery("b")]);
    assert!(first.delivered(&delivery("b")).is_none());
    assert!(second.delivered(&delivery("b")).unwrap().is_ok());
    assert!(first.delivered(&delivery("a")).unwrap().is_ok());
    assert!(
        first.delivered(&delivery("a")).is_none(),
        "acknowledged once"
    );
    assert_eq!(seen.borrow().acknowledged, [delivery("a")]);
    assert_eq!(other.borrow().acknowledged, [delivery("b")]);
}

#[test]
fn holds_final_deliveries() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut script = Script::new(&seen);
    script.deliveries = vec![delivery("a")];
    let mut child = Child::new(Box::new(script));
    assert_eq!(child.finish().unwrap(), Some(json!({"out": 1})));
    assert_eq!(
        child.targets(),
        ["/out/a"],
        "the results may name what it delivers"
    );
    assert_eq!(child.deliveries(), [delivery("a")], "held for the parent");
    assert!(child.deliveries().is_empty(), "handed out once");
    assert!(child.delivered(&delivery("a")).unwrap().is_ok());
    assert_eq!(seen.borrow().acknowledged, [delivery("a")]);
}

#[test]
fn checks_child_inputs() {
    let schema = json!({
        "type": "object",
        "properties": {"lines": {"type": "integer"}},
        "required": ["lines"]
    });
    let seen = Rc::new(RefCell::new(Seen::default()));
    let mut script = Script::new(&seen);
    script.inputs = Some(Inputs {
        schema: schema.clone(),
        values: json!({"lines": "three"}),
        ..Inputs::default()
    });
    let mut child = Child::new(Box::new(script));
    let error = child.prepare(None, None, false).unwrap_err();
    assert!(error.message.contains("lines"), "{error}");
    assert_eq!(seen.borrow().used, None);
    let mut script = Script::new(&seen);
    script.inputs = Some(Inputs {
        schema,
        values: json!({"lines": 3}),
        ..Inputs::default()
    });
    let mut child = Child::new(Box::new(script));
    child.prepare(None, None, false).unwrap();
    assert_eq!(seen.borrow().used, Some(json!({"lines": 3})));
}
