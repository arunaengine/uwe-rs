//! Checks lock admission, atomic acquisition and configured semaphore limits.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;
use std::time::Duration;

use workflow_graph::Outcome;

use crate::{
    Options,
    library::{Document, Kind, Library},
    run::Run,
};

use super::Locks;

fn run(control: &str, library: Rc<Library>) -> Run {
    let source = format!(
        "kind: Workflow\nmetadata: {{name: locks}}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    steps:\n    - - {{name: a, template: work}}\n      - {{name: b, template: work}}\n      - {{name: c, template: work}}\n  - name: work\n    synchronization: {control}\n    container: {{image: debian:stable-slim}}\n"
    );
    let document = Rc::new(Document {
        path: "locks.yaml".into(),
        index: 0,
        kind: Kind::Workflow,
        name: "locks".into(),
        node: argo_parser::parse_yaml(&source).unwrap().unwrap(),
    });
    Run::new(
        document,
        library,
        Options {
            assume_inputs: true,
            ..Options::default()
        },
    )
    .unwrap()
}

#[test]
fn mutex_serializes_calls() {
    // A database mutex locks as a local one does inside one controller.
    for control in [
        "{mutexes: [{name: one}]}",
        "{mutexes: [{name: one, database: true}]}",
    ] {
        let mut run = run(control, Rc::new(Library::new()));
        for name in ["a", "b", "c"] {
            let tasks = run.operate().unwrap();
            assert_eq!(tasks.len(), 1, "{control}");
            assert!(tasks[0].name.ends_with(name));
            let next = run
                .complete(Outcome {
                    id: tasks[0].id,
                    ..Outcome::default()
                })
                .unwrap();
            run.emitted.extend(next);
        }
        assert!(run.result().is_ok());
    }
}

#[test]
fn semaphore_bounds_calls() {
    for control in [
        "{semaphores: [{configMapKeyRef: {name: capacity, key: jobs, limit: 2}}]}",
        "{semaphores: [{configMapKeyRef: {name: capacity, key: jobs}}]}",
    ] {
        let library = Rc::new(Library::new());
        library.add_documents(
            0,
            [Rc::new(Document {
                path: "locks.yaml".into(),
                index: 1,
                kind: Kind::ConfigMap,
                name: "capacity".into(),
                node: argo_parser::parse_yaml(
                    "kind: ConfigMap\nmetadata: {name: capacity}\ndata: {jobs: '2'}",
                )
                .unwrap()
                .unwrap(),
            })],
        );
        let mut run = run(control, library);
        let first = run.operate().unwrap();
        assert_eq!(first.len(), 2);
        let next = run
            .complete(Outcome {
                id: first[1].id,
                ..Outcome::default()
            })
            .unwrap();
        assert_eq!(next.len(), 1);
        for task in [first[0].id, next[0].id] {
            assert!(
                run.complete(Outcome {
                    id: task,
                    ..Outcome::default()
                })
                .unwrap()
                .is_empty()
            );
        }
        assert!(run.result().is_ok());
    }
}

#[test]
fn locks_acquire_together() {
    let mut locks = Locks::default();
    let a = vec![("a".into(), 1)];
    let both = vec![("a".into(), 1), ("b".into(), 1)];
    assert!(locks.acquire("first", &a).unwrap());
    assert!(!locks.acquire("second", &both).unwrap());
    assert!(locks.0["b"].holders.is_empty());
    locks.release("first");
    assert!(locks.acquire("second", &both).unwrap());
    locks.release("second");
    assert!(locks.acquire("third", &both).unwrap());
}

#[test]
fn retry_retains_lock() {
    let mut run = run(
        "{mutexes: [{name: one}]}\n    retryStrategy: {limit: 1, backoff: {duration: 2s}}",
        Rc::new(Library::new()),
    );
    let first = run.operate().unwrap().remove(0);
    assert!(
        run.complete(Outcome {
            id: first.id,
            exit_code: 1,
            ..Outcome::default()
        })
        .unwrap()
        .is_empty()
    );
    run.clock += Duration::from_secs(2);
    let retry = run.operate().unwrap().remove(0);
    assert!(retry.name.contains(".a(1)"));
    assert_eq!(
        run.complete(Outcome {
            id: retry.id,
            ..Outcome::default()
        })
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn missing_names_key() {
    let mut run = run(
        "{semaphores: [{configMapKeyRef: {name: missing, key: jobs}}]}",
        Rc::new(Library::new()),
    );
    let error = run.operate().unwrap_err();
    assert!(error.unsupported);
    assert!(error.message.contains("default/missing key jobs"));
}

#[test]
fn workflows_share_locks() {
    let library = Rc::new(Library::new());
    let mut left = run("{}", library.clone());
    let mut right = run("{}", library);
    right.workflow_name = "right".into();
    right.locks = left.locks.clone();
    let sync = argo_parser::parse_yaml("{mutexes: [{name: shared}]}")
        .unwrap()
        .unwrap();
    left.spec.synchronization = Some(sync.clone());
    right.spec.synchronization = Some(sync);
    let tasks = left.operate().unwrap();
    assert_eq!(tasks.len(), 3);
    assert!(right.operate().unwrap().is_empty());
    for task in tasks {
        left.complete(Outcome {
            id: task.id,
            ..Outcome::default()
        })
        .unwrap();
    }
    assert_eq!(right.operate().unwrap().len(), 3);
}

#[test]
fn resolves_namespace_limits() {
    let library = Library::new();
    for (namespace, value) in [("a", "1"), ("b", "2")] {
        library.add_documents(
            0,
            [Rc::new(Document {
                path: "capacity.yaml".into(),
                index: usize::from(namespace == "b"),
                kind: Kind::ConfigMap,
                name: "capacity".into(),
                node: argo_parser::parse_yaml(&format!(
                    "metadata: {{namespace: {namespace}}}\ndata: {{jobs: '{value}'}}\n"
                ))
                .unwrap()
                .unwrap(),
            })],
        );
    }
    let node =
        argo_parser::parse_yaml("{semaphores: [{configMapKeyRef: {name: capacity, key: jobs}}]}")
            .unwrap()
            .unwrap();
    for (namespace, expected) in [("a", 1), ("b", 2)] {
        let limits =
            super::limits(&node, &library, std::path::Path::new("run.yaml"), namespace).unwrap();
        assert_eq!(
            limits,
            vec![(format!("{namespace}/semaphore/capacity/jobs"), expected)]
        );
    }
}
