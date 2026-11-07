//! Checks the child driver adapter: task ids across retries, acceptance, deliveries, inputs
//! and a single finish.
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
