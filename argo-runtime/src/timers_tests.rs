//! Checks deadline decisions and cancellation cleanup through the engine.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use workflow_graph::{Attempt, ExecuteError, Executor, Outcome, Running, TaskError, execute};

use crate::library::{Document, Kind, Library};
use crate::node::Phase;
use crate::run::Run;
use crate::{ArgoDriver, Options};

fn document(fields: &str) -> Rc<Document> {
    let source =
        format!("kind: Workflow\nmetadata: {{name: limits}}\nspec:\n  entrypoint: main\n{fields}");
    Rc::new(Document {
        path: "limits.yaml".into(),
        index: 0,
        kind: Kind::Workflow,
        name: "limits".into(),
        node: argo_parser::parse_yaml(&source).unwrap().unwrap(),
    })
}

fn run(fields: &str) -> Run {
    Run::new(
        document(fields),
        Rc::new(Library::new()),
        Options {
            assume_inputs: true,
            ..Options::default()
        },
    )
    .unwrap()
}

#[test]
fn deadline_covers_init() {
    let mut run = run(
        "  templates:\n  - name: main\n    activeDeadlineSeconds: 5\n    initContainers: [{name: init, image: debian:stable-slim}]\n    container: {image: debian:stable-slim}\n",
    );
    let first = run.operate().unwrap().remove(0);
    let deadline = run.deadlines[&0].at;
    run.clock += Duration::from_secs(3);
    let next = run
        .complete(Outcome {
            id: first.id,
            ..Outcome::default()
        })
        .unwrap()
        .remove(0);
    assert_eq!(run.deadlines[&0].at, deadline);
    assert!(run.wake(deadline).unwrap().is_empty());
    assert_eq!(run.cancellations(), vec![next.id]);
    assert_eq!(run.nodes[0].phase, Phase::Running);
    run.complete(Outcome {
        id: next.id,
        error: Some(TaskError::Cancelled),
        ..Outcome::default()
    })
    .unwrap();
    assert_eq!(run.nodes[0].phase, Phase::Failed);
    assert!(run.nodes[0].message.contains("specified deadline"));
}

#[test]
fn workflow_runs_exit() {
    let mut run = run(
        "  activeDeadlineSeconds: 1\n  onExit: exit\n  templates:\n  - name: main\n    container: {image: debian:stable-slim}\n  - name: exit\n    container: {image: debian:stable-slim}\n",
    );
    let task = run.operate().unwrap().remove(0);
    let deadline = run.workflow_limit.unwrap();
    assert!(run.wake(deadline).unwrap().is_empty());
    assert_eq!(run.cancellations(), vec![task.id]);
    let exit = run
        .complete(Outcome {
            id: task.id,
            error: Some(TaskError::Cancelled),
            ..Outcome::default()
        })
        .unwrap()
        .remove(0);
    assert_eq!(exit.step, "exit");
    run.complete(Outcome {
        id: exit.id,
        ..Outcome::default()
    })
    .unwrap();
    assert!(
        run.result()
            .unwrap_err()
            .contains("Step exceeded its deadline")
    );
    assert!(run.wake_at().is_none());
}

#[test]
fn bounds_retry_attempt() {
    let mut run = run(
        "  templates:\n  - name: main\n    activeDeadlineSeconds: 1\n    retryStrategy: {limit: 1, backoff: {duration: 0s, maxDuration: 5s}}\n    container: {image: debian:stable-slim}\n",
    );
    let first = run.operate().unwrap().remove(0);
    let second = run
        .complete(Outcome {
            id: first.id,
            exit_code: 1,
            ..Outcome::default()
        })
        .unwrap()
        .remove(0);
    let node = run.tasks[&second.id].0;
    assert_eq!(second.resources.time_seconds, Some(5));
    assert_eq!(
        run.deadlines[&node].at,
        run.nodes[1].started_at + Duration::from_secs(5)
    );
    let deadline = run.deadlines[&node].at;
    run.wake(deadline).unwrap();
    run.complete(Outcome {
        id: second.id,
        error: Some(TaskError::Cancelled),
        ..Outcome::default()
    })
    .unwrap();
    assert!(
        run.result()
            .unwrap_err()
            .contains("Max duration limit exceeded")
    );
}

struct Waiting(Arc<AtomicBool>);

impl Executor for Waiting {
    fn run(&mut self, attempt: Attempt) -> Running {
        let cleaned = self.0.clone();
        Running::new(async move {
            attempt.cancel.cancelled().await;
            cleaned.store(true, Ordering::SeqCst);
            Err(ExecuteError::Cancelled)
        })
    }
}

#[test]
fn engine_waits_cleanup() {
    let cleaned = Arc::new(AtomicBool::new(false));
    let mut driver = ArgoDriver::new(
        document(
            "  templates:\n  - name: main\n    timeout: 10ms\n    container: {image: debian:stable-slim}\n",
        ),
        Rc::new(Library::new()),
        Options::default(),
    );
    let result = execute("limits", &mut driver, &mut Waiting(cleaned.clone()));
    assert!(cleaned.load(Ordering::SeqCst));
    assert!(result.error.unwrap().to_string().contains("timeout"));
}

#[test]
fn timed_suspend_waits() {
    let mut run = run("  templates:\n  - name: main\n    suspend: {duration: 20ms}\n");
    assert!(run.operate().unwrap().is_empty());
    let deadline = run.waits[&0];
    run.clock += Duration::from_millis(19);
    assert!(run.operate().unwrap().is_empty());
    assert_eq!(run.nodes[0].phase, Phase::Running);
    run.wake(deadline).unwrap();
    assert_eq!(run.nodes[0].phase, Phase::Succeeded);
    assert!(run.result().is_ok());
}

#[test]
fn suspend_allows_siblings() {
    let mut run = run(
        "  templates:\n  - name: main\n    steps:\n    - - {name: wait, template: wait}\n      - {name: work, template: work}\n    - - {name: after, template: work}\n  - name: wait\n    suspend: {duration: 5s}\n  - name: work\n    container: {image: debian:stable-slim}\n",
    );
    let tasks = run.operate().unwrap();
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0].name.ends_with("work"));
    assert!(
        run.complete(Outcome {
            id: tasks[0].id,
            ..Outcome::default()
        })
        .unwrap()
        .is_empty()
    );
    let deadline = *run.waits.values().next().unwrap();
    let next = run.wake(deadline).unwrap();
    assert_eq!(next.len(), 1);
    assert!(next[0].name.ends_with("after"));
    assert_eq!(next[0].after, vec![tasks[0].id]);
}

#[test]
fn retry_pending_expires() {
    let mut run = run(
        "  templates:\n  - name: main\n    steps:\n    - - {name: holder, template: holder}\n      - {name: waiter, template: waiter}\n  - name: holder\n    synchronization: {mutexes: [{name: shared}]}\n    container: {image: debian:stable-slim}\n  - name: waiter\n    timeout: 1s\n    retryStrategy: {limit: 1}\n    synchronization: {mutexes: [{name: shared}]}\n    container: {image: debian:stable-slim}\n",
    );
    let first = run.operate().unwrap();
    assert_eq!(first.len(), 1);
    let deadline = run.deadlines.values().map(|limit| limit.at).min().unwrap();
    assert!(run.wake(deadline).unwrap().is_empty());
    assert!(run.cancellations().is_empty());
    assert!(
        run.complete(Outcome {
            id: first[0].id,
            ..Outcome::default()
        })
        .unwrap()
        .is_empty()
    );
    assert!(run.result().unwrap_err().contains("timeout"));
}

#[test]
fn timeout_cancels_retry() {
    let mut run = run(
        "  templates:\n  - name: main\n    timeout: 1s\n    retryStrategy: {limit: 2}\n    container: {image: debian:stable-slim}\n",
    );
    let first = run.operate().unwrap().remove(0);
    let origin = run.clock;
    run.clock += Duration::from_millis(500);
    let second = run
        .complete(Outcome {
            id: first.id,
            exit_code: 1,
            ..Outcome::default()
        })
        .unwrap()
        .remove(0);
    assert!(
        run.wake(origin + Duration::from_secs(1))
            .unwrap()
            .is_empty()
    );
    assert_eq!(run.cancellations(), vec![second.id]);
    assert_eq!(run.nodes[0].phase, Phase::Running);
    assert!(
        run.complete(Outcome {
            id: second.id,
            error: Some(TaskError::Cancelled),
            ..Outcome::default()
        })
        .unwrap()
        .is_empty()
    );
    assert_eq!(run.nodes[0].message, "timeout");
    assert_eq!(run.nodes[0].children.len(), 2);
}
