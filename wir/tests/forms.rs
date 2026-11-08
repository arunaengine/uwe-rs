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

/// A document of one graph `g` with these ops and edges.
fn document(ops: Json, edges: Json) -> Json {
    json!({"wir_version": wir::VERSION,
        "graphs": [{"name": "g", "ops": ops, "edges": edges}], "entry": "g"})
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
