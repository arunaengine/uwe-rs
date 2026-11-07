//! Checks the state of a run: calls, `after` dependencies, failures and draining.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;

use super::*;
use crate::plan::plan;

/// A graph where `first` feeds `second` and `cleanup` runs only after `first` failed. Each
/// of the three calls the graph `leaf`, which runs one op on its input.
fn progress() -> Progress {
    let op = |path: &str, after: Json| {
        json!({"path": path, "kind": {"call": {"graph": "leaf"}},
            "inputs": [{"name": "x", "type": "int"}], "outputs": [{"name": "y", "type": "int"}],
            "after": after})
    };
    let leaf = json!({"name": "leaf", "inputs": [{"name": "x", "type": "int"}],
        "outputs": [{"name": "y", "type": "int"}],
        "ops": [{"path": "work", "kind": {"native": {"source": "s", "language": "cwl"}},
            "inputs": [{"name": "x", "type": "int"}], "outputs": [{"name": "y", "type": "int"}]}],
        "edges": [{"from": {"port": "x"}, "to": {"op": "work", "port": "x"}},
            {"from": {"op": "work", "port": "y"}, "to": {"port": "y"}}]});
    let main = json!({"name": "main", "inputs": [{"name": "x", "type": "int"}],
        "outputs": [{"name": "y", "type": "int"}],
        "ops": [op("first", json!([])), op("second", json!([])),
            op("cleanup", json!([{"op": "first", "on": ["failed"]}]))],
        "edges": [{"from": {"port": "x"}, "to": {"op": "first", "port": "x"}},
            {"from": {"op": "first", "port": "y"}, "to": {"op": "second", "port": "x"}},
            {"from": {"port": "x"}, "to": {"op": "cleanup", "port": "x"}},
            {"from": {"op": "second", "port": "y"}, "to": {"port": "y"}}]});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "s", "language": "cwl", "location": "s.cwl"}],
        "graphs": [main, leaf], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    wir::verify(&document).unwrap();
    let mut progress = Progress::new(plan(&document).unwrap(), Vec::new());
    progress.inputs.insert("x".into(), json!(2));
    progress
}

/// The node of a path.
fn at(progress: &Progress, path: &str) -> usize {
    (progress.plan.nodes.iter())
        .position(|node| node.path == path)
        .unwrap()
}

#[test]
fn calls_bind_outputs() {
    let mut progress = progress();
    let (first, bound) = progress.next(false).unwrap();
    assert_eq!(progress.node(first).path, "first");
    assert_eq!(bound, Map::from_iter([("x".into(), json!(2))]));
    let (work, bound) = progress.next(false).unwrap();
    assert_eq!(progress.node(work).path, "first/work");
    assert_eq!(
        bound["x"],
        json!(2),
        "a call passes its inputs to its graph"
    );
    assert_eq!(progress.next(false), None, "second waits for first");
    let values = Map::from_iter([("y".into(), json!(5))]);
    progress.end(work, Status::Succeeded, values, None);
    let (second, bound) = progress.next(false).unwrap();
    assert_eq!(progress.node(second).path, "second");
    assert_eq!(bound["x"], json!(5), "the call bound its graph's output");
    let (work, _) = progress.next(false).unwrap();
    assert_eq!(progress.node(work).path, "second/work");
    let omitted = State::Ended(Status::Omitted, Map::new());
    assert_eq!(progress.states[at(&progress, "cleanup")], omitted);
    progress.end(
        work,
        Status::Succeeded,
        Map::from_iter([("y".into(), json!(7))]),
        None,
    );
    assert_eq!(progress.next(false), None);
    assert_eq!(progress.states[at(&progress, "cleanup/work")], omitted);
    assert_eq!(progress.outputs().unwrap()["y"], json!(7));
    assert!(progress.failures.is_empty());
}

#[test]
fn handles_failed_dependencies() {
    let mut progress = progress();
    progress.next(false).unwrap();
    let (work, _) = progress.next(false).unwrap();
    progress.end(
        work,
        Status::Failed,
        Map::new(),
        Some("op first/work: broke".into()),
    );
    let (cleanup, _) = progress.next(false).unwrap();
    assert_eq!(
        progress.node(cleanup).path,
        "cleanup",
        "a dependency on failure holds"
    );
    let second = at(&progress, "second");
    assert_eq!(
        progress.states[second],
        State::Ended(Status::Omitted, Map::new())
    );
    assert_eq!(
        progress.states[at(&progress, "first")],
        State::Ended(Status::Failed, Map::new())
    );
    assert!(
        progress.failures.is_empty(),
        "cleanup handles the failure of first"
    );
    assert!(progress.outputs().is_err());
}

#[test]
fn calls_report_failures() {
    let mut progress = progress();
    let first = progress.next(false).unwrap().0;
    let (work, _) = progress.next(false).unwrap();
    progress.end(
        work,
        Status::Succeeded,
        Map::from_iter([("y".into(), json!(5))]),
        None,
    );
    progress.next(false).unwrap();
    let (work, _) = progress.next(false).unwrap();
    progress.end(
        work,
        Status::Errored,
        Map::new(),
        Some("op second/work: lost".into()),
    );
    assert_eq!(progress.next(false), None);
    assert_eq!(
        progress.states[first],
        State::Ended(Status::Succeeded, Map::from_iter([("y".into(), json!(5))]))
    );
    assert_eq!(progress.failures, ["op second/work: lost"]);
}

#[test]
fn draining_starts_nothing() {
    let mut progress = progress();
    assert_eq!(progress.next(true), None);
    assert_eq!(progress.states[0], State::Waiting);
}
