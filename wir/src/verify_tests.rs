//! Tests of the verifier: one valid document and one change per problem.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};

use super::*;

fn task(path: &str) -> Json {
    json!({
        "path": path,
        "kind": {"task": {
            "command": ["cat", {"input": "input"}],
            "captures": [{"output": "out", "from": "stdout"}]
        }},
        "inputs": [{"name": "input", "type": {"file": {}}}],
        "outputs": [{"name": "out", "type": {"file": {}}}]
    })
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// Two chained tasks: `text` -> sort -> head -> `top`.
fn base() -> Json {
    json!({
        "wir_version": VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "text", "type": {"file": {}}}],
            "outputs": [{"name": "top", "type": {"file": {}}}],
            "ops": [task("sort"), task("head")],
            "edges": [
                edge((None, "text"), (Some("sort"), "input")),
                edge((Some("sort"), "out"), (Some("head"), "input")),
                edge((Some("head"), "out"), (None, "top"))
            ]
        }],
        "entry": "main"
    })
}

fn problems(document: Json) -> Vec<VerifyError> {
    let document: Document = serde_json::from_value(document).expect("a parsable document");
    verify(&document).err().unwrap_or_default()
}

fn only(document: Json) -> (String, Problem) {
    let found = problems(document);
    assert_eq!(found.len(), 1, "one problem expected: {found:?}");
    (found[0].at.clone(), found[0].problem.clone())
}

#[test]
fn valid_document_passes() {
    assert_eq!(problems(base()), Vec::new());
}

#[test]
fn rejects_unknown_version() {
    let mut document = base();
    document["wir_version"] = json!("0.0");
    assert_eq!(
        only(document),
        ("document".into(), Problem::Version("0.0".into()))
    );
}

#[test]
fn paths_are_unique() {
    let mut document = base();
    document["graphs"][0]["ops"][1]["path"] = json!("sort");
    let found = problems(document);
    assert!(found.contains(&VerifyError {
        at: "main".into(),
        problem: Problem::Duplicate("sort".into())
    }));
    let mut document = base();
    document["graphs"][0]["ops"][1]["path"] = json!("steps/head two");
    assert!(
        problems(document)
            .iter()
            .any(|e| e.problem == Problem::BadName("steps/head two".into()))
    );
}

#[test]
fn edge_types_match() {
    let mut document = base();
    document["graphs"][0]["ops"][1]["inputs"][0]["type"] = json!("string");
    let (at, problem) = only(document);
    assert_eq!(at, "main/head");
    assert!(matches!(problem, Problem::Mismatch { port, .. } if port == "input input"));
    let mut document = base();
    document["graphs"][0]["ops"][1]["inputs"][0]["flow"] = json!("stream");
    assert_eq!(
        only(document),
        (
            "main/head".into(),
            Problem::FlowMismatch("input input".into())
        )
    );
}

#[test]
fn one_producer_each() {
    let mut document = base();
    let extra = edge((None, "text"), (Some("head"), "input"));
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .push(extra);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::Producers("input input".into()))
    );
}

#[test]
fn required_inputs_bound() {
    let mut document = base();
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .remove(1);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::Unbound("input input".into()))
    );
    let mut document = base();
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .remove(1);
    document["graphs"][0]["ops"][1]["inputs"][0]["type"] = json!({"optional": {"file": {}}});
    assert_eq!(problems(document), Vec::new());
}

#[test]
fn rejects_op_cycles() {
    let mut document = base();
    let back = json!({"op": "head", "on": ["succeeded"]});
    document["graphs"][0]["ops"][0]["after"] = json!([back]);
    let (at, problem) = only(document);
    assert!(at.starts_with("main/"));
    assert!(matches!(problem, Problem::Cycle(_)));
}

#[test]
fn rejects_graph_recursion() {
    let mut document = base();
    let call = json!({"path": "again", "kind": {"call": {"graph": "main"}},
        "inputs": [{"name": "text", "type": {"file": {}}}],
        "outputs": [{"name": "top", "type": {"file": {}}}]});
    document["graphs"][0]["ops"]
        .as_array_mut()
        .expect("ops")
        .push(call);
    let edge = edge((None, "text"), (Some("again"), "text"));
    document["graphs"][0]["edges"]
        .as_array_mut()
        .expect("edges")
        .push(edge);
    assert_eq!(
        only(document),
        ("main".into(), Problem::Recursion("main".into()))
    );
}

#[test]
fn call_ports_match() {
    let mut document = base();
    let inner = document["graphs"][0].clone();
    document["graphs"][0]["name"] = json!("inner");
    let call = json!({"path": "run", "kind": {"call": {"graph": "inner"}},
        "inputs": [{"name": "text", "type": "string"}],
        "outputs": [{"name": "top", "type": {"file": {}}}]});
    let outer = json!({"name": "main", "inputs": inner["inputs"], "outputs": inner["outputs"],
    "ops": [call], "edges": [
        edge((None, "text"), (Some("run"), "text")),
        edge((Some("run"), "top"), (None, "top"))
    ]});
    document["graphs"]
        .as_array_mut()
        .expect("graphs")
        .push(outer);
    let found = problems(document);
    let wanted = Problem::CallPort {
        port: "input text".into(),
        graph: "inner".into(),
    };
    assert!(
        found
            .iter()
            .any(|e| e.at == "main/run" && e.problem == wanted),
        "{found:?}"
    );
}

#[test]
fn native_ports_typed() {
    let mut document = base();
    document["sources"] = json!([{"id": "flow", "language": "nextflow", "location": "main.nf"}]);
    document["graphs"][0]["ops"][1]["kind"] =
        json!({"native": {"source": "flow", "language": "cwl"}});
    document["graphs"][0]["ops"][1]["outputs"][0]["type"] = json!("dynamic");
    let found: Vec<Problem> = problems(document).into_iter().map(|e| e.problem).collect();
    assert!(
        found.contains(&Problem::Untyped("port out".into())),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|p| matches!(p, Problem::Language { named, .. } if named == "flow"))
    );
}

#[test]
fn task_outputs_captured() {
    let mut document = base();
    document["graphs"][0]["ops"][1]["kind"]["task"]["captures"] = json!([]);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::Capture("out".into()))
    );
    let mut document = base();
    document["graphs"][0]["ops"][1]["outputs"][0]["type"] = json!("int");
    document["graphs"][0]["outputs"][0]["type"] = json!("int");
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::CaptureType("out".into()))
    );
    let mut document = base();
    document["graphs"][0]["ops"][1]["kind"]["task"]["command"] = json!([{"input": "other"}]);
    assert_eq!(
        only(document),
        ("main/head".into(), Problem::NoPort("input other".into()))
    );
}

#[test]
fn rejects_recursive_types() {
    let mut document = base();
    document["types"] = json!([{"name": "Tree", "type": {"array": {"items": {"named": "Tree"}}}}]);
    assert_eq!(
        only(document),
        ("types/Tree".into(), Problem::RecursiveType("Tree".into()))
    );
    let mut document = base();
    document["graphs"][0]["inputs"][0]["type"] = json!({"named": "Missing"});
    let found = problems(document);
    assert!(
        found
            .iter()
            .any(|e| e.problem == Problem::NoType("Missing".into())),
        "{found:?}"
    );
}
