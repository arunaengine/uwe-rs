//! Checks the state of a run: calls, `after` dependencies, failures, draining, widening and
//! the check of `dynamic` values.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;

use super::*;
use crate::adapter::Adapters;
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
    let mut progress = Progress::new(plan(&document, &Adapters::default()).unwrap(), Vec::new());
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

/// A call `c` of the graph `inner`, whose native ops are `a` and the extra `ops`.
fn handled_call(ops: Json) -> Progress {
    let native = json!({"path": "a", "kind": {"native": {"source": "s", "language": "cwl"}}});
    let mut inner = vec![native];
    inner.extend(ops.as_array().cloned().unwrap_or_default());
    let call = json!({"path": "c", "kind": {"call": {"graph": "inner"}}});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "s", "language": "cwl", "location": "s.cwl"}],
        "graphs": [{"name": "main", "ops": [call]}, {"name": "inner", "ops": inner}],
        "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    wir::verify(&document).unwrap();
    Progress::new(plan(&document, &Adapters::default()).unwrap(), Vec::new())
}

#[test]
fn calls_handle_failures() {
    let native =
        |path: &str| json!({"path": path, "kind": {"native": {"source": "s", "language": "cwl"}}});
    let mut handler = native("b");
    handler["handles"] = json!([{"op": "a", "on": ["failed"]}]);
    let group =
        json!({"path": "g", "kind": {"status_group": {"members": ["a"], "allow": ["failed"]}}});
    for ops in [json!([handler]), json!([group])] {
        let mut progress = handled_call(ops);
        progress.next(false).unwrap();
        while let Some((node, _)) = progress.next(false) {
            let status = match progress.node(node).path.as_str() {
                "c/a" => Status::Failed,
                _ => Status::Succeeded,
            };
            progress.end(node, status, Map::new(), Some("op c/a: broke".into()));
        }
        assert_eq!(
            status_of(&progress, "c"),
            Some(Status::Succeeded),
            "the failure is handled"
        );
        assert!(progress.failures.is_empty());
    }
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
    (
        Progress::new(plan(&document, &Adapters::default()).unwrap(), Vec::new()),
        ports,
    )
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
    let mut progress = Progress::new(plan(&document, &Adapters::default()).unwrap(), Vec::new());
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

/// A graph `main` of native ops `a`, `b` and `c` and of `extra`, all without ports, with
/// the fields of `changes` set on them.
fn controlled(changes: Json, extra: Json) -> Progress {
    let op =
        |path: &str| json!({"path": path, "kind": {"native": {"source": "s", "language": "cwl"}}});
    let mut ops = vec![op("a"), op("b"), op("c")];
    ops.extend(extra.as_array().cloned().unwrap_or_default());
    for item in &mut ops {
        if let Some(fields) = changes.get(item["path"].as_str().unwrap()) {
            item.as_object_mut()
                .unwrap()
                .extend(fields.as_object().cloned().unwrap());
        }
    }
    let document = json!({"wir_version": "0.1", "sources": [{"id": "s", "language": "cwl", "location": "s.cwl"}],
        "graphs": [{"name": "main", "ops": ops}], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    wir::verify(&document).unwrap();
    Progress::new(plan(&document, &Adapters::default()).unwrap(), Vec::new())
}

/// Starts the node `path`, which must be next, and ends it with `status`.
fn run_as(progress: &mut Progress, path: &str, status: Status) {
    let (node, _) = progress.next(false).unwrap();
    assert_eq!(progress.node(node).path, path);
    progress.end(
        node,
        status,
        Map::new(),
        Some(format!("op {path}: {status:?}")),
    );
}

fn status_of(progress: &Progress, path: &str) -> Option<Status> {
    match &progress.states[at(progress, path)] {
        State::Ended(status, _) => Some(*status),
        _ => None,
    }
}

#[test]
fn control_boolean() {
    let test = |op: &str, on: &str| json!({"result": {"op": op, "on": [on]}});
    let tree = json!({"any": [test("a", "failed"), {"not": test("b", "succeeded")}]});
    let mut progress = controlled(json!({"c": {"depends": tree}}), json!([]));
    run_as(&mut progress, "a", Status::Succeeded);
    let (b, _) = progress.next(false).unwrap();
    assert_eq!(progress.node(b).path, "b");
    assert_eq!(
        progress.next(false),
        None,
        "c waits: not on a running op is pending"
    );
    progress.end(b, Status::Succeeded, Map::new(), None);
    assert_eq!(progress.next(false), None);
    assert_eq!(
        status_of(&progress, "c"),
        Some(Status::Omitted),
        "the tree is false"
    );
    let empty = controlled(
        json!({"a": {"depends": {"all": []}}, "b": {"depends": {"any": []}}}),
        json!([]),
    );
    let mut empty = empty;
    run_as(&mut empty, "a", Status::Succeeded);
    let (c, _) = empty.next(false).unwrap();
    assert_eq!(empty.node(c).path, "c");
    assert_eq!(
        status_of(&empty, "b"),
        Some(Status::Omitted),
        "an empty any is false, an empty all true"
    );
}

#[test]
fn control_waits() {
    let test = |op: &str, on: &str| json!({"result": {"op": op, "on": [on]}});
    let tree = json!({"not": {"all": [test("a", "failed"), test("b", "succeeded")]}});
    let mut progress = controlled(json!({"c": {"depends": tree}}), json!([]));
    run_as(&mut progress, "a", Status::Succeeded);
    let (b, _) = progress.next(false).unwrap();
    assert_eq!(
        progress.next(false),
        None,
        "c waits for the result of b, though a decides the tree"
    );
    progress.end(b, Status::Succeeded, Map::new(), None);
    let (c, _) = progress.next(false).unwrap();
    assert_eq!(progress.node(c).path, "c");
}

#[test]
fn control_groups() {
    let group = json!({"path": "g", "kind": {"status_group": {"members": ["a", "b"], "allow": ["errored"]}}});
    let tests = json!({"c": {"depends": {"result": {"op": "g", "on": ["any_succeeded"]}}}});
    let mut progress = controlled(tests, json!([group]));
    run_as(&mut progress, "a", Status::Errored);
    run_as(&mut progress, "b", Status::Succeeded);
    let (c, _) = progress.next(false).unwrap();
    assert_eq!(progress.node(c).path, "c");
    assert_eq!(
        status_of(&progress, "g"),
        Some(Status::Succeeded),
        "an allowed error does not count"
    );
    let empty = json!({"path": "g", "kind": {"status_group": {"members": []}}});
    let failed = json!({"c": {"depends": {"result": {"op": "g", "on": ["all_failed"]}}}});
    let mut progress = controlled(failed, json!([empty]));
    run_as(&mut progress, "a", Status::Succeeded);
    run_as(&mut progress, "b", Status::Succeeded);
    assert_eq!(progress.next(false), None);
    assert_eq!(
        status_of(&progress, "g"),
        Some(Status::Succeeded),
        "an empty group succeeds"
    );
    assert_eq!(
        status_of(&progress, "c"),
        Some(Status::Omitted),
        "all failed needs members"
    );
}

#[test]
fn failure_handler() {
    let handles = json!({"b": {"handles": [{"op": "a", "on": ["failed"]}], "after": [{"op": "a", "on": ["failed"]}]}});
    let mut progress = controlled(handles.clone(), json!([]));
    run_as(&mut progress, "a", Status::Failed);
    assert!(
        progress.failures.is_empty(),
        "the failure waits for its handler"
    );
    run_as(&mut progress, "b", Status::Succeeded);
    run_as(&mut progress, "c", Status::Succeeded);
    assert!(
        progress.failures.is_empty(),
        "a handler that succeeds handles it"
    );
    let mut progress = controlled(handles, json!([]));
    run_as(&mut progress, "a", Status::Failed);
    run_as(&mut progress, "b", Status::Failed);
    assert_eq!(
        progress.failures,
        ["op a: Failed", "op b: Failed"],
        "a failed handler handles nothing"
    );
}

#[test]
fn scheduler_work() {
    let rounds = 500;
    let int = |name: &str| json!({"name": name, "type": "int"});
    let ints = |name: &str| json!({"name": name, "type": {"array": {"items": "int"}}});
    let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
    let work = json!({"path": "work", "kind": {"native": {"source": "s", "language": "cwl"}},
        "inputs": [int("x")], "outputs": [int("y")]});
    let body = json!({"name": "body", "inputs": [int("x")], "outputs": [int("y")], "ops": [work],
        "edges": [edge((None, "x"), (Some("work"), "x")), edge((Some("work"), "y"), (None, "y"))]});
    let each = json!({"path": "each", "kind": {"map": {"body": "body", "over": ["x"]}},
        "inputs": [ints("x")], "outputs": [ints("y")]});
    let main = json!({"name": "main", "inputs": [ints("xs")], "outputs": [ints("ys")], "ops": [each],
        "edges": [edge((None, "xs"), (Some("each"), "x")), edge((Some("each"), "y"), (None, "ys"))]});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "s", "language": "cwl", "location": "s.cwl"}],
        "graphs": [main, body], "entry": "main"});
    let document = wir::parse(&document.to_string()).unwrap();
    wir::verify(&document).unwrap();
    let mut progress = Progress::new(plan(&document, &Adapters::default()).unwrap(), Vec::new());
    progress.inputs.insert("xs".into(), json!(vec![1; rounds]));
    progress.activate(None).unwrap();
    let (map, bound) = progress.next(false).unwrap();
    let Kind::Map(spec) = progress.node(map).op.kind.clone() else {
        panic!("no map");
    };
    let items = crate::plan::items(&spec, &bound).unwrap();
    let start = progress.plan.nodes.len();
    crate::plan::rounds(&document, &mut progress.plan.nodes, map, "body", items).unwrap();
    progress.plan.nodes[map].shape = vec![rounds];
    progress.adopt(start);
    while let Some((at, _)) = progress.next(false) {
        match progress.node(at).op.kind {
            Kind::Call(_) => progress.activate(Some(at)).unwrap(),
            _ => progress.end(
                at,
                Status::Succeeded,
                Map::from_iter([("y".into(), json!(2))]),
                None,
            ),
        }
    }
    let State::Ended(Status::Succeeded, values) = &progress.states[map] else {
        panic!("the map did not end: {:?}", progress.states[map]);
    };
    assert_eq!(values["y"].as_array().map(Vec::len), Some(rounds));
    let nodes = progress.plan.nodes.len() as u64;
    assert!(
        progress.visits <= 8 * nodes,
        "{} visits for {nodes} nodes: the scheduler looks at each node a few times",
        progress.visits
    );
}
