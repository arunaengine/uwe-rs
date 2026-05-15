//! The engine keeps the order rules of a run: a task starts only after the tasks in its `after`
//! list have finished, and it may only name tasks that the driver asked for before. With run
//! options, several tasks are pending at once and finish in another order.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use workflow_graph::{
    Driver, DriverError, EdgeKind, ExecuteError, Executor, Language, Order, Outcome, RunOptions,
    Task, TaskId, execute, execute_with,
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

/// Runs the script with the options; the result has the order in which tasks started and the
/// order in which the driver heard of them.
fn run(mut driver: Script, max_pending: usize, order: Order) -> (Vec<TaskId>, Vec<TaskId>) {
    let mut executor = Log::default();
    let options = RunOptions { max_pending, order };
    execute_with("toy", &mut driver, &mut executor, &options)
        .into_result()
        .expect("the run succeeds");
    (executor.started, driver.heard)
}

fn ids(numbers: &[u64]) -> Vec<TaskId> {
    numbers.iter().copied().map(TaskId).collect()
}

/// Five tasks at the start, and two more when task 2 finishes.
fn fan_out() -> Script {
    Script {
        first: (1..=5).map(|id| task(id, &[])).collect(),
        then: HashMap::from([(TaskId(2), vec![task(6, &[]), task(7, &[])])]),
        ..Script::default()
    }
}

#[test]
fn the_default_options_run_tasks_as_asked() {
    let (started, heard) = run(fan_out(), 1, Order::AsAsked);
    assert_eq!(started, ids(&[1, 2, 3, 4, 5, 6, 7]));
    assert_eq!(heard, started);
    let mut driver = fan_out();
    execute("toy", &mut driver, &mut Log::default());
    assert_eq!(driver.heard, heard);
}

#[test]
fn pending_tasks_finish_in_reverse() {
    let (started, heard) = run(fan_out(), 3, Order::Reversed);
    assert_eq!(started, ids(&[1, 2, 3, 4, 5, 6, 7]));
    // 1, 2 and 3 are pending; 3 finishes, 4 starts and finishes, then 5; then 2, which asks
    // for 6 and 7.
    assert_eq!(heard, ids(&[3, 4, 5, 2, 7, 6, 1]));
}

#[test]
fn shuffled_orders_repeat_with_their_seed() {
    let (_, first) = run(fan_out(), 4, Order::Shuffled { seed: 7 });
    let (_, again) = run(fan_out(), 4, Order::Shuffled { seed: 7 });
    assert_eq!(first, again);
    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(sorted, ids(&[1, 2, 3, 4, 5, 6, 7]));
    let orders: Vec<Vec<TaskId>> = (0..8)
        .map(|seed| run(fan_out(), 4, Order::Shuffled { seed }).1)
        .collect();
    assert!(orders.iter().any(|order| *order != orders[0]));
}

#[test]
fn pending_tasks_keep_the_after_rule() {
    let driver = Script {
        first: vec![task(1, &[]), task(2, &[1]), task(3, &[])],
        ..Script::default()
    };
    let (started, heard) = run(driver, 3, Order::Reversed);
    // Task 2 cannot start before task 1 has finished, although there is room for it.
    assert_eq!(started, ids(&[1, 3, 2]));
    assert_eq!(heard, ids(&[3, 1, 2]));
}
