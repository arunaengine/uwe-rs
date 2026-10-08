//! Checks deadline decisions and cancellation cleanup through the engine.
// Copyright (c) 2026 The iunctus Contributors
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
