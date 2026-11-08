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

fn port(name: &str, r#type: &str) -> Json {
    json!({"name": name, "type": r#type})
}

/// A graph `main` whose op `keep` filters the input `n` with `body`, capturing `limit`.
fn filtering(body: Json, callback: Json) -> Json {
    let keep = json!({"path": "keep", "kind": {"filter": {"body": body}},
        "inputs": [port("n", "int"), port("limit", "int")], "outputs": [port("out", "int")]});
    let main = json!({"name": "main", "inputs": [port("n", "int"), port("limit", "int")],
        "outputs": [port("kept", "int")], "ops": [keep],
        "edges": [edge((None, "n"), (Some("keep"), "n")),
            edge((None, "limit"), (Some("keep"), "limit")),
            edge((Some("keep"), "out"), (None, "kept"))]});
    let sources = json!([{"id": "nf", "language": "nextflow", "location": "main.nf"},
        {"id": "cwl", "language": "cwl", "location": "main.cwl"}]);
    let mut graphs = vec![main];
    graphs.extend(callback.as_array().cloned().unwrap_or_default());
    json!({"wir_version": wir::VERSION, "sources": sources, "graphs": graphs, "entry": "main"})
}

/// A callback graph `small` that compares `item` with the capture `limit`.
fn small(outputs: Json, ops: Json) -> Json {
    let compare = json!({"path": "lt", "kind": {"expr": {"language": "js",
        "code": "$(inputs.item < inputs.limit)"}},
        "inputs": [port("item", "int"), port("limit", "int")], "outputs": [port("out", "bool")]});
    let mut all = vec![compare];
    all.extend(ops.as_array().cloned().unwrap_or_default());
    json!({"name": "small", "inputs": [port("item", "int"), port("limit", "int")],
        "outputs": outputs, "ops": all,
        "edges": [edge((None, "item"), (Some("lt"), "item")),
            edge((None, "limit"), (Some("lt"), "limit")), edge((Some("lt"), "out"), (None, "out"))]})
}

#[test]
fn callback_contract() {
    let good = filtering(
        json!("small"),
        json!([small(json!([port("out", "bool")]), json!([]))]),
    );
    assert_eq!(problems(&good), Vec::<String>::new());
    let named = filtering(
        json!("small"),
        json!([small(json!([port("ok", "bool")]), json!([]))]),
    );
    let found = problems(&named);
    assert!(
        found.contains(&"main/keep: callback small: a callback graph has one output, out".into()),
        "{found:?}"
    );
    let typed = filtering(
        json!("small"),
        json!([small(json!([port("out", "int")]), json!([]))]),
    );
    let found = problems(&typed);
    assert!(
        found.iter().any(|problem| problem.contains("is wanted")),
        "{found:?}"
    );
    let task = json!({"path": "t", "kind": {"task": {"command": ["true"]}}});
    let effects = filtering(
        json!("small"),
        json!([small(json!([port("out", "bool")]), json!([task]))]),
    );
    reports(
        &effects,
        "main/keep: callback small: it runs small/t, which is not pure",
    );
    let mut stranger = filtering(
        json!("small"),
        json!([small(json!([port("out", "bool")]), json!([]))]),
    );
    stranger["graphs"][1]["inputs"][1]["name"] = json!("other");
    stranger["graphs"][1]["edges"][1]["from"]["port"] = json!("other");
    reports(
        &stranger,
        "main/keep: callback small: capture other names no value input of the op",
    );
    let closure = |extra: Json| {
        let mut body = json!({"language": "groovy", "code": "{ it < limit }",
            "captures": {"limit": {"input": "limit"}}});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap());
        filtering(body, json!([]))
    };
    assert_eq!(problems(&closure(json!({}))), Vec::<String>::new());
    let missing = closure(json!({"captures": {"limit": {"input": "cap"}}}));
    reports(
        &missing,
        "main/keep: closure: capture limit names no value input cap",
    );
    reports(
        &closure(json!({"language": "js"})),
        "main/keep: closure: js is no closure language; use javascript",
    );
    let wdl = closure(json!({"language": "wdl", "code": "x < limit"}));
    reports(&wdl, "main/keep: closure: a wdl closure names 1 parameters");
    let clash = closure(json!({"language": "wdl", "parameters": ["limit"]}));
    reports(
        &clash,
        "main/keep: closure: capture limit is also a parameter",
    );
}

#[test]
fn scope_bounds() {
    let scoped = |scope: Json| {
        let body = json!({"language": "groovy", "code": "{ it > 1 }", "scope": scope});
        filtering(body, json!([]))
    };
    let good = scoped(json!({"source": "nf", "start": 4, "end": 9}));
    assert_eq!(problems(&good), Vec::<String>::new());
    let reversed = scoped(json!({"source": "nf", "start": 9, "end": 4}));
    reports(
        &reversed,
        "main/keep: closure: the scope 9..4 ends before it starts",
    );
    let other = scoped(json!({"source": "cwl", "start": 0, "end": 1}));
    reports(
        &other,
        "main/keep: language \"groovy\" differs from that of source cwl, \"cwl\"",
    );
    let unknown = scoped(json!({"source": "nope", "start": 0, "end": 1}));
    reports(&unknown, "main/keep: no source named nope");
}
