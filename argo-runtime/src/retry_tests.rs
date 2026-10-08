//! Checks retry delays, duration bounds and policy classification with explicit time.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;
use std::time::Duration;

use workflow_graph::{Outcome, TaskError};

use crate::Options;
use crate::library::{Document, Kind, Library};
use crate::node::Phase;
use crate::run::Run;

fn run(strategy: &str) -> Run {
    let source = format!(
        "kind: Workflow\nmetadata: {{name: retry}}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    retryStrategy: {{{strategy}}}\n    container: {{image: debian:stable-slim}}\n"
    );
    let document = Rc::new(Document {
        path: "retry.yaml".into(),
        index: 0,
        kind: Kind::Workflow,
        name: "retry".into(),
        node: argo_parser::parse_yaml(&source).unwrap().unwrap(),
    });
    Run::new(
        document,
        Rc::new(Library::new()),
        Options {
            assume_inputs: true,
            ..Options::default()
        },
    )
    .unwrap()
}

#[test]
fn delays_and_caps() {
    let mut run = run("limit: 3, backoff: {duration: 2s, factor: 3, cap: 5s}");
    let first = run.operate().unwrap().remove(0);
    let origin = run.clock;
    assert!(
        run.complete(Outcome {
            id: first.id,
            exit_code: 1,
            ..Outcome::default()
        })
        .unwrap()
        .is_empty()
    );
    assert_eq!(run.waits[&0], origin + Duration::from_secs(2));
    run.clock += Duration::from_secs(1);
    assert!(run.operate().unwrap().is_empty());
    run.clock += Duration::from_secs(1);
    let second = run.operate().unwrap().remove(0);
    assert!(
        run.complete(Outcome {
            id: second.id,
            exit_code: 1,
            ..Outcome::default()
        })
        .unwrap()
        .is_empty()
    );
    assert_eq!(run.waits[&0], origin + Duration::from_secs(7));
    run.clock += Duration::from_secs(5);
    let third = run.operate().unwrap().remove(0);
    run.complete(Outcome {
        id: third.id,
        ..Outcome::default()
    })
    .unwrap();
    assert!(run.waits.is_empty());
    assert!(run.result().is_ok());
}

#[test]
fn bounds_max_duration() {
    for (elapsed, message) in [
        (0, "Backoff would exceed max duration limit"),
        (3, "Max duration limit exceeded"),
    ] {
        let mut run = run("limit: 2, backoff: {duration: 5s, maxDuration: 3s}");
        let first = run.operate().unwrap().remove(0);
        run.clock += Duration::from_secs(elapsed);
        assert!(
            run.complete(Outcome {
                id: first.id,
                exit_code: 1,
                ..Outcome::default()
            })
            .unwrap()
            .is_empty()
        );
        assert_eq!(run.nodes[0].message, message);
        assert_eq!(run.nodes[0].phase, Phase::Failed);
        assert!(run.waits.is_empty());
    }
}

#[test]
fn classifies_retry_errors() {
    for (policy, error, expected) in [
        ("Always", None, true),
        ("Always", Some(TaskError::System), true),
        ("OnFailure", None, true),
        ("OnFailure", Some(TaskError::Transient), false),
        ("OnError", None, false),
        ("OnError", Some(TaskError::System), true),
        ("OnTransientError", Some(TaskError::System), false),
        ("OnTransientError", Some(TaskError::Transient), true),
    ] {
        let mut run = run(&format!("limit: 1, retryPolicy: {policy}"));
        let task = run.operate().unwrap().remove(0);
        let next = run
            .complete(Outcome {
                id: task.id,
                exit_code: 1,
                error,
                ..Outcome::default()
            })
            .unwrap();
        assert_eq!(next.len(), usize::from(expected), "{policy} {error:?}");
    }
}

#[test]
fn preserves_fractional_delay() {
    let backoff = argo_parser::parse_yaml("{duration: 20ms, factor: 2}")
        .unwrap()
        .unwrap();
    assert_eq!(
        super::backoff_delay(&backoff, 1).unwrap(),
        Duration::from_millis(20)
    );
    assert_eq!(
        super::backoff_delay(&backoff, 2).unwrap(),
        Duration::from_millis(40)
    );
}
