//! Native acceptance and asynchronous settlement are bound to one task attempt.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use workflow_graph::{
    Attempt, Driver, DriverError, ExecuteError, Executor, Language, Outcome, Running, Task, TaskId,
};

struct Defaults;

impl Driver for Defaults {
    fn language(&self) -> Language {
        Language::Cwl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(Vec::new())
    }

    fn complete(&mut self, _outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

impl Executor for Defaults {
    fn run(&mut self, _attempt: Attempt) -> Running {
        Running::ended(Ok(Outcome::default()))
    }
}

#[tokio::test]
async fn success_without_explicit_acceptance_is_rejected_and_settlement_defaults_to_no_op() {
    let mut defaults = Defaults;
    let driver: &mut dyn Driver = &mut defaults;
    driver
        .complete(Outcome {
            id: TaskId(7),
            attempt: 2,
            exit_code: 0,
            ..Outcome::default()
        })
        .unwrap();
    for (task, attempt) in [(TaskId(7), 2), (TaskId(7), 1), (TaskId(8), 2)] {
        assert!(!driver.take_acceptance(task, attempt));
    }
    let executor: &mut dyn Executor = &mut defaults;
    executor.settle(TaskId(7), 2, false).await.unwrap();
    executor.settle(TaskId(7), 2, true).await.unwrap();
}

#[derive(Default)]
struct Explicit {
    decisions: HashMap<(TaskId, u32), bool>,
    pending: HashMap<(TaskId, u32), ()>,
    settled: Arc<Mutex<HashMap<(TaskId, u32), bool>>>,
}

impl Driver for Explicit {
    fn language(&self) -> Language {
        Language::Cwl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(Vec::new())
    }

    fn complete(&mut self, _outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.decisions.remove(&(task, attempt)).unwrap_or(false)
    }
}

impl Executor for Explicit {
    fn run(&mut self, _attempt: Attempt) -> Running {
        Running::ended(Ok(Outcome::default()))
    }

    fn settle(
        &mut self,
        task: TaskId,
        attempt: u32,
        accepted: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecuteError>> + Send + 'static>> {
        let pending = self.pending.remove(&(task, attempt)).is_some();
        let settled = self.settled.clone();
        Box::pin(async move {
            if pending {
                settled.lock().unwrap().insert((task, attempt), accepted);
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn decisions_and_owned_settlement_futures_keep_both_identity_components() {
    let first = (TaskId(7), 1);
    let retry = (TaskId(7), 2);
    let other = (TaskId(8), 2);
    let mut explicit = Explicit {
        decisions: HashMap::from([(first, false), (retry, true), (other, true)]),
        pending: HashMap::from([(first, ()), (retry, ()), (other, ())]),
        ..Explicit::default()
    };
    let driver: &mut dyn Driver = &mut explicit;
    assert!(!driver.take_acceptance(TaskId(8), 1));
    assert!(!driver.take_acceptance(first.0, first.1));
    assert!(driver.take_acceptance(retry.0, retry.1));
    assert!(!driver.take_acceptance(retry.0, retry.1));
    assert!(driver.take_acceptance(other.0, other.1));

    let settled = explicit.settled.clone();
    let executor: &mut dyn Executor = &mut explicit;
    executor.settle(TaskId(8), 1, true).await.unwrap();
    executor.settle(first.0, first.1, false).await.unwrap();
    executor.settle(first.0, first.1, true).await.unwrap();
    let retry_future = executor.settle(retry.0, retry.1, true);
    let other_future = executor.settle(other.0, other.1, false);
    drop(explicit);
    tokio::spawn(retry_future).await.unwrap().unwrap();
    tokio::spawn(other_future).await.unwrap().unwrap();
    assert_eq!(
        *settled.lock().unwrap(),
        HashMap::from([(first, false), (retry, true), (other, false)])
    );
}
