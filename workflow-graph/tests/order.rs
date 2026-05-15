//! The engine keeps the order rules of a run: a task starts only after the tasks in its `after`
//! list have finished, and it may only name tasks that the driver asked for before.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use workflow_graph::{
    Driver, DriverError, EdgeKind, ExecuteError, Executor, Language, Outcome, Task, TaskId, execute,
};

/// Asks for fixed tasks: some at the start, others when a given task finishes. It notes the
/// order in which it hears of outcomes.
#[derive(Default)]
struct Script {
    first: Vec<Task>,
    then: HashMap<TaskId, Vec<Task>>,
    heard: Vec<TaskId>,
}

impl Driver for Script {
    fn language(&self) -> Language {
        Language::Argo
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(std::mem::take(&mut self.first))
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.heard.push(outcome.id);
        Ok(self.then.remove(&outcome.id).unwrap_or_default())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Succeeds at once and notes the order in which tasks start.
#[derive(Default)]
struct Log {
    started: Vec<TaskId>,
}

impl Executor for Log {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        self.started.push(task.id);
        Ok(Outcome {
            id: task.id,
            ..Outcome::default()
        })
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

#[test]
fn after_is_an_order_edge() {
    let mut driver = Script {
        first: vec![task(1, &[]), task(2, &[1])],
        ..Script::default()
    };
    let graph = execute("toy", &mut driver, &mut Log::default())
        .into_result()
        .expect("the run succeeds");
    graph.check().expect("a valid graph");
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].kind, EdgeKind::Order);
    assert_eq!(driver.heard, [TaskId(1), TaskId(2)]);
}

#[test]
fn after_names_only_tasks_asked_for_before() {
    let cases: [(Vec<Task>, u64, u64); 3] = [
        // A task that never comes.
        (vec![task(1, &[9])], 1, 9),
        // A task asked for later in the same list.
        (vec![task(1, &[2]), task(2, &[])], 1, 2),
        // The task itself.
        (vec![task(1, &[1])], 1, 1),
    ];
    for (first, waiting, missing) in cases {
        let mut driver = Script {
            first,
            ..Script::default()
        };
        let error = execute("toy", &mut driver, &mut Log::default())
            .error
            .expect("an error");
        assert!(
            matches!(error, ExecuteError::UnknownAfter { task, after }
                if task == TaskId(waiting) && after == TaskId(missing)),
            "{error}"
        );
    }
}
