//! Tests for the translation of `depends` and `dependencies`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use argo_parser::model::{ContinueOn, DagTask};

use super::dependencies;

fn task(name: &str, depends: &str, needs: &[&str]) -> DagTask {
    DagTask {
        name: name.to_owned(),
        depends: depends.to_owned(),
        dependencies: needs.iter().map(|need| (*need).to_owned()).collect(),
        ..DagTask::default()
    }
}

#[test]
fn bare_names_expand() {
    let tasks = vec![
        task("a", "", &[]),
        task("b-1", "", &[]),
        task("c", "a && (b-1.Failed || b-1.Errored)", &[]),
    ];
    let found = dependencies(&tasks[2], &tasks);
    assert_eq!(found.names, ["a", "b-1"]);
    assert_eq!(
        found.logic,
        "(a.Succeeded || a.Skipped || a.Daemoned) && (b-1.Failed || b-1.Errored)"
    );
}

#[test]
fn honours_continue_dependencies() {
    let mut a = task("a", "", &[]);
    a.continue_on = Some(ContinueOn {
        error: false,
        failed: true,
    });
    let tasks = vec![a, task("b", "", &[]), task("c", "", &["a", "b"])];
    let found = dependencies(&tasks[2], &tasks);
    assert_eq!(
        found.logic,
        "(a.Succeeded || a.Skipped || a.Daemoned || a.Failed) && (b.Succeeded || b.Skipped || b.Daemoned)"
    );
}

#[test]
fn qualified_results_stay() {
    let tasks = vec![
        task("loop", "", &[]),
        task("next", "loop.AnySucceeded || !loop.AllFailed", &[]),
    ];
    let found = dependencies(&tasks[1], &tasks);
    assert_eq!(found.names, ["loop"]);
    assert_eq!(found.logic, "loop.AnySucceeded || !loop.AllFailed");
}
