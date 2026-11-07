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
