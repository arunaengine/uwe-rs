//! Checks the state of a run: calls, `after` dependencies, failures, draining, widening and
//! the check of `dynamic` values.
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
    assert_eq!(progress.outputs(&[]).unwrap()["y"], json!(7));
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
    assert!(progress.outputs(&[]).is_err());
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

#[test]
fn widening_keeps_items() {
    let pair = Type::Tuple(vec![Type::Float, Type::Int]);
    assert_eq!(widen(json!([1, 2]), &pair, &[]), json!([1.0, 2]));
    let widened = widen(json!([1, 2, 3]), &pair, &[]);
    assert_eq!(widened, json!([1.0, 2, 3]));
    assert!(
        !wir::fits(&widened, &pair, &[]),
        "three items do not fit a pair"
    );
}

/// A Nextflow block whose `dynamic` output feeds an `int` input and an `int` graph output,
/// with the output ports of the graph.
fn dynamic_progress() -> (Progress, Vec<wir::Port>) {
    let main = json!({"name": "main", "inputs": [], "outputs": [{"name": "z", "type": "int"}],
        "ops": [{"path": "emit", "kind": {"native": {"source": "n", "language": "nextflow"}},
                "inputs": [], "outputs": [{"name": "y", "type": "dynamic"}]},
            {"path": "use", "kind": {"native": {"source": "c", "language": "cwl"}},
                "inputs": [{"name": "x", "type": "int"}], "outputs": []}],
        "edges": [{"from": {"op": "emit", "port": "y"}, "to": {"op": "use", "port": "x"}},
            {"from": {"op": "emit", "port": "y"}, "to": {"port": "z"}}]});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "n", "language": "nextflow", "location": "main.nf"},
            {"id": "c", "language": "cwl", "location": "c.cwl"}],
        "graphs": [main], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    wir::verify(&document).unwrap();
    let ports = document.graphs[0].outputs.clone();
    (Progress::new(plan(&document).unwrap(), Vec::new()), ports)
}

#[test]
fn checks_dynamic_values() {
    let (mut progress, ports) = dynamic_progress();
    let (emit, _) = progress.next(false).unwrap();
    progress.end(
        emit,
        Status::Succeeded,
        Map::from_iter([("y".into(), json!("7"))]),
        None,
    );
    assert_eq!(progress.next(false), None, "use gets no text for its int");
    let failed = State::Ended(Status::Failed, Map::new());
    assert_eq!(progress.states[at(&progress, "use")], failed);
    assert!(
        progress.failures[0].contains("input x does not fit"),
        "{:?}",
        progress.failures
    );
    let error = progress.outputs(&ports).unwrap_err();
    assert!(error.contains("output z does not fit"), "{error}");
    let (mut progress, ports) = dynamic_progress();
    let (emit, _) = progress.next(false).unwrap();
    progress.end(
        emit,
        Status::Succeeded,
        Map::from_iter([("y".into(), json!(7))]),
        None,
    );
    let (used, bound) = progress.next(false).unwrap();
    assert_eq!(
        (progress.node(used).path.as_str(), &bound["x"]),
        ("use", &json!(7))
    );
    assert_eq!(progress.outputs(&ports).unwrap()["z"], json!(7));
}

#[test]
fn value_empty() {
    let mut progress = progress();
    progress.next(false).unwrap();
    let (work, _) = progress.next(false).unwrap();
    progress.end_with(work, Status::Succeeded, Map::new(), vec!["y".into()]);
    assert_eq!(
        progress.next(false),
        None,
        "an empty value starts no reader"
    );
    let first = &progress.states[at(&progress, "first")];
    assert_eq!(
        *first,
        State::Ended(Status::Succeeded, Map::new()),
        "the call closes empty too"
    );
    let second = &progress.states[at(&progress, "second")];
    assert_eq!(*second, State::Ended(Status::Omitted, Map::new()));
    assert!(progress.failures.is_empty());
    assert!(
        progress.outputs(&[]).is_err(),
        "an omitted producer binds no output"
    );
}

#[test]
fn empty_outputs_omitted() {
    let mut progress = progress();
    progress.next(false).unwrap();
    let (work, _) = progress.next(false).unwrap();
    progress.end(
        work,
        Status::Succeeded,
        Map::from_iter([("y".into(), Json::Null)]),
        None,
    );
    let (second, bound) = progress.next(false).unwrap();
    assert_eq!(progress.node(second).path, "second");
    assert_eq!(bound["x"], Json::Null, "a bound null is a value");
    let (work, _) = progress.next(false).unwrap();
    progress.end_with(work, Status::Succeeded, Map::new(), vec!["y".into()]);
    assert_eq!(progress.next(false), None);
    assert_eq!(
        progress.outputs(&[]).unwrap(),
        Map::new(),
        "an empty output is left out"
    );
}

/// A graph `main` whose call `wrap` passes the stream input `xs` on to its output, and whose
/// graph also runs an op `work` that may fail.
fn fenced() -> Progress {
    let stream = |name: &str| json!({"name": name, "type": "int", "flow": "stream"});
    let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
    let work = json!({"path": "work", "kind": {"native": {"source": "s", "language": "cwl"}}});
    let pass = json!({"name": "pass", "inputs": [stream("x")], "outputs": [stream("y")], "ops": [work],
        "edges": [edge((None, "x"), (None, "y"))]});
    let wrap = json!({"path": "wrap", "kind": {"call": {"graph": "pass"}}, "inputs": [stream("x")], "outputs": [stream("y")]});
    let main = json!({"name": "main", "inputs": [stream("xs")], "outputs": [stream("ys")], "ops": [wrap],
        "edges": [edge((None, "xs"), (Some("wrap"), "x")), edge((Some("wrap"), "y"), (None, "ys"))]});
    let document = json!({"wir_version": "0.1", "sources": [{"id": "s", "language": "cwl", "location": "s.cwl"}],
        "graphs": [main, pass], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    wir::verify(&document).unwrap();
    let mut progress = Progress::new(plan(&document).unwrap(), Vec::new());
    let values = Map::from_iter([("xs".into(), json!([1, 2]))]);
    progress.feed(&values, &["xs".into()]).unwrap();
    progress.activate(None).unwrap();
    progress
}

/// How the stream output `y` of `wrap` ended, if it did.
fn wrapped_end(progress: &Progress) -> Option<crate::streams::StreamEnd> {
    let channel = &progress.streams.channels[&crate::flows::output_stream("wrap", "y")];
    channel
        .terminal
        .as_ref()
        .map(|terminal| terminal.end.clone())
}

#[test]
fn boundary_close_fence() {
    let mut progress = fenced();
    let (wrap, _) = progress.next(false).unwrap();
    progress.activate(Some(wrap)).unwrap();
    let (work, _) = progress.next(false).unwrap();
    assert_eq!(progress.node(work).path, "wrap/work");
    assert_eq!(progress.next(false), None);
    let channel = &progress.streams.channels[&crate::flows::output_stream("wrap", "y")];
    assert_eq!(channel.next, 2, "the items pass before the graph ends");
    assert_eq!(wrapped_end(&progress), None, "the close waits for the call");
    progress.end(
        work,
        Status::Failed,
        Map::new(),
        Some("op wrap/work: broke".into()),
    );
    assert_eq!(progress.next(false), None);
    let Some(crate::streams::StreamEnd::Error(failure)) = wrapped_end(&progress) else {
        panic!("a failure inside the call ends its output with an error");
    };
    assert_eq!(failure.message, "op wrap/work: broke");
    let mut progress = fenced();
    let (wrap, _) = progress.next(false).unwrap();
    progress.activate(Some(wrap)).unwrap();
    let (work, _) = progress.next(false).unwrap();
    progress.end(work, Status::Succeeded, Map::new(), None);
    assert_eq!(progress.next(false), None);
    assert_eq!(
        wrapped_end(&progress),
        Some(crate::streams::StreamEnd::Close)
    );
    let ports = [wir::Port {
        name: "ys".into(),
        r#type: wir::Type::Int,
        flow: wir::Flow::Stream,
        default: None,
    }];
    assert_eq!(progress.outputs(&ports).unwrap()["ys"], json!([1, 2]));
}
