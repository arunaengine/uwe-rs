//! Checks the JSON form of the stream format: defaults left out, fields kept, and the schema
//! and the parser accepting and rejecting the same documents.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jsonschema::{Draft, Validator};
use serde_json::{Value as Json, json};

fn validator() -> Validator {
    let schema: Json = serde_json::from_str(wir::SCHEMA).expect("the schema is JSON");
    (jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema))
    .expect("a validator")
}

/// A document of one graph `g` with these ops and edges; empty lists are left out.
fn document(ops: Json, edges: Json) -> Json {
    let mut graph = json!({"name": "g"});
    for (key, list) in [("ops", ops), ("edges", edges)] {
        if list.as_array().is_some_and(|items| !items.is_empty()) {
            graph[key] = list;
        }
    }
    json!({"wir_version": wir::VERSION, "graphs": [graph], "entry": "g"})
}

/// Whether the schema and the parser both accept the document; panics when they disagree.
fn accepted(document: &Json) -> bool {
    let schema = validator().is_valid(document);
    let parsed = wir::parse(&document.to_string());
    assert_eq!(
        schema,
        parsed.is_ok(),
        "schema and parser differ on {document}: {parsed:?}"
    );
    schema
}

/// The document after parsing and writing it again.
fn written(document: &Json) -> Json {
    let parsed = wir::parse(&document.to_string()).expect("a document");
    serde_json::to_value(parsed).expect("serializes")
}

#[test]
fn subscription_forms() {
    let edge = |read: Json| {
        let mut edge = json!({"from": {"op": "a", "port": "out"}, "to": {"op": "b", "port": "in"}});
        if !read.is_null() {
            edge["read"] = read;
        }
        document(json!([]), json!([edge]))
    };
    let all = edge(json!("all"));
    assert!(accepted(&all));
    let stored = &written(&all)["graphs"][0]["edges"][0];
    assert!(stored.get("read").is_none(), "all is the default: {stored}");
    let live = edge(json!("live"));
    assert!(accepted(&live));
    assert_eq!(written(&live)["graphs"][0]["edges"][0]["read"], "live");
    assert!(!accepted(&edge(json!("late"))));
}

#[test]
fn body_forms() {
    let graph: wir::Body = serde_json::from_value(json!("square")).expect("a graph body");
    assert_eq!(graph.graph(), Some("square"));
    let closure = json!({"language": "groovy", "code": "{ x -> x * 2 }",
        "scope": {"source": "main", "start": 10, "end": 42},
        "captures": {"factor": {"input": "factor"}, "base": {"value": [1, 2]}}});
    let body: wir::Body = serde_json::from_value(closure.clone()).expect("a closure body");
    assert_eq!(body.graph(), None);
    assert_eq!(serde_json::to_value(&body).expect("serializes"), closure);
    let unknown = json!({"language": "groovy", "code": "{ it }", "frame": 1});
    assert!(serde_json::from_value::<wir::Body>(unknown).is_err());
    let binding =
        json!({"captures": {"x": {"input": "a", "value": 1}}, "language": "wdl", "code": "x"});
    assert!(serde_json::from_value::<wir::Body>(binding).is_err());
}

#[test]
fn expr_closure_fields() {
    let expr = |language: &str| {
        let op = json!({"path": "e", "kind": {"expr": {"language": language, "code": "1",
            "captures": {"n": {"value": 3}}}}, "values": "groovy",
            "outputs": [{"name": "out", "type": "int"}]});
        document(json!([op]), json!([]))
    };
    for language in ["js", "javascript", "groovy", "python", "wdl"] {
        let document = expr(language);
        assert!(accepted(&document), "{language}");
        assert_eq!(written(&document), document);
    }
    assert!(!accepted(&expr("perl")));
    let mut plain = expr("js");
    plain["graphs"][0]["ops"][0]["values"] = json!("wir");
    let stored = &written(&plain)["graphs"][0]["ops"][0];
    assert!(
        stored.get("values").is_none(),
        "wir is the default: {stored}"
    );
}

/// Each kind parses, fits the schema, and is written back as it was; each wrong form fails.
fn kinds_round_trip(good: &[Json], bad: &[Json]) {
    for kind in good {
        let document = document(json!([{"path": "a", "kind": kind}]), json!([]));
        assert!(accepted(&document), "{kind}");
        assert_eq!(written(&document), document, "{kind}");
    }
    for kind in bad {
        let document = document(json!([{"path": "a", "kind": kind}]), json!([]));
        assert!(!accepted(&document), "{kind}");
    }
}

#[test]
fn transform_forms() {
    let good = [
        json!({"emit": {}}),
        json!({"emit": {"each": true}}),
        json!({"transform": {"body": "double"}}),
        json!({"transform": {"body": {"language": "groovy", "code": "{ it }"}, "nulls": "keep"}}),
        json!({"filter": {"body": "small"}}),
        json!({"flat_map": {}}),
        json!({"flat_map": {"body": "split"}}),
        json!({"flatten": {}}),
        json!({"flatten": {"depth": "all", "drop_null": true}}),
    ];
    let bad = [
        json!({"emit": {"each": "yes"}}),
        json!({"transform": {}}),
        json!({"transform": {"body": "double", "nulls": "skip"}}),
        json!({"flatten": {"depth": 2}}),
    ];
    kinds_round_trip(&good, &bad);
    let defaults = json!({"transform": {"body": "double", "nulls": "drop"}});
    let document = document(json!([{"path": "a", "kind": defaults}]), json!([]));
    let stored = &written(&document)["graphs"][0]["ops"][0]["kind"]["transform"];
    assert_eq!(*stored, json!({"body": "double"}));
}
