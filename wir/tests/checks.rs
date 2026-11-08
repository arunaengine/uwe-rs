//! Checks the verifier rules of the stream format, one document per rule.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};

/// The problems the verifier finds in a document, as text.
fn problems(document: &Json) -> Vec<String> {
    let parsed = wir::parse(&document.to_string()).expect("a parsable document");
    match wir::verify(&parsed) {
        Ok(()) => Vec::new(),
        Err(found) => found.iter().map(ToString::to_string).collect(),
    }
}

/// Whether the verifier reports exactly `expected`.
fn reports(document: &Json, expected: &str) {
    assert_eq!(problems(document), [expected], "{document}");
}

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

fn stream(name: &str) -> Json {
    json!({"name": name, "type": "int", "flow": "stream"})
}

/// A graph `main` that passes its stream input `xs` through a call of the graph `pass`.
fn passing() -> Json {
    let pass = json!({"name": "pass", "inputs": [stream("x")], "outputs": [stream("y")],
        "edges": [edge((None, "x"), (None, "y"))]});
    let call = json!({"path": "p", "kind": {"call": {"graph": "pass"}},
        "inputs": [stream("x")], "outputs": [stream("y")]});
    let main = json!({"name": "main", "inputs": [stream("xs")], "outputs": [stream("ys")],
        "ops": [call],
        "edges": [edge((None, "xs"), (Some("p"), "x")), edge((Some("p"), "y"), (None, "ys"))]});
    json!({"wir_version": wir::VERSION, "graphs": [main, pass], "entry": "main"})
}

#[test]
fn stream_flow_rules() {
    assert_eq!(problems(&passing()), Vec::<String>::new());
    let mut defaulted = passing();
    defaulted["graphs"][0]["ops"][0]["inputs"][0]["default"] = json!(1);
    reports(
        &defaulted,
        "main/p: input x is a stream and takes no default",
    );
    let mut unfed = passing();
    unfed["graphs"][0]["edges"][0] = edge((None, "xs"), (None, "ys"));
    unfed["graphs"][0]["edges"].as_array_mut().unwrap().pop();
    reports(
        &unfed,
        "main/p: input x is a stream input without a producer",
    );
    let mut live = passing();
    live["graphs"][1]["inputs"][0]["flow"] = json!("value");
    live["graphs"][1]["outputs"][0]["flow"] = json!("value");
    live["graphs"][1]["edges"][0]["read"] = json!("live");
    let found = problems(&live);
    assert!(
        found.contains(&"pass: graph output y reads a value, which has no live reader".into()),
        "{found:?}"
    );
}
