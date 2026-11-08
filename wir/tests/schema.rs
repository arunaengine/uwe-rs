//! Checks the JSON Schema: it fits draft 2020-12 and accepts and rejects what the parser does.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jsonschema::{Draft, Validator};
use serde_json::{Value as Json, json};

fn validator() -> Validator {
    let schema: Json = serde_json::from_str(wir::SCHEMA).expect("the schema is JSON");
    jsonschema::draft202012::meta::validate(&schema).expect("the schema fits draft 2020-12");
    (jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema))
    .expect("a validator")
}

/// Every op kind in its smallest form fits the schema and parses, so the two stay in step.
#[test]
fn schema_matches_parser() {
    let kinds = [
        json!({"task": {"command": ["true"], "success_codes": [0, 1]}}),
        json!({"task": {"command": ["true"], "captures": [
            {"output": "o", "from": {"glob": "*.txt"}, "load_contents": true}
        ]}}),
        json!({"call": {"graph": "g"}}),
        json!({"native": {"source": "s", "language": "argo"}}),
        json!({"native": {"source": "s", "language": "nextflow", "entry": "w",
            "profiles": ["test"], "configs": ["c"]}}),
        json!({"expr": {"language": "js", "code": "1 + 1"}}),
        json!({"map": {"body": "g", "over": ["a"], "mode": "flat_cross", "min_success": 1}}),
        json!({"collect": {}}),
        json!({"filter": {"body": "g"}}),
        json!({"flatten": {}}),
        json!({"merge": {}}),
        json!({"zip": {"remainder": true}}),
        json!({"cross": {}}),
        json!({"group": {"by": [0], "size": 2}}),
        json!({"join": {"by": [0], "fail_on_mismatch": true}}),
        json!({"branch": {"body": "g", "condition": "c"}}),
        json!({"select": {"rule": "only"}}),
        json!({"loop": {"body": "g", "condition": "c", "carry": [{"output": "o", "input": "i"}]}}),
        json!({"dynamic": {"plan": "p"}}),
    ];
    let validator = validator();
    for kind in kinds {
        let op = json!({"path": "a/b", "kind": kind, "after": [{"op": "c", "on": ["failed"]}],
            "effects": ["nondeterministic", {"publish": "results"}]});
        let document = json!({"wir_version": wir::VERSION, "graphs": [{"name": "g", "ops": [op]}], "entry": "g"});
        assert!(validator.is_valid(&document), "schema rejects {kind}");
        assert!(
            wir::parse(&document.to_string()).is_ok(),
            "parser rejects {kind}"
        );
    }
    let wrong = json!({"wir_version": wir::VERSION, "graphs": [{"name": "g", "ops": [
        {"path": "a", "kind": {"collect": {}, "merge": {}}}
    ]}], "entry": "g"});
    assert!(!validator.is_valid(&wrong));
    assert!(wir::parse(&wrong.to_string()).is_err());
}

#[test]
fn schema_rejects_unknown() {
    let validator = validator();
    let op = json!({"path": "a", "kind": {"call": {"graph": "g"}}});
    let mut value =
        json!({"wir_version": wir::VERSION, "graphs": [{"name": "g", "ops": [op]}], "entry": "g"});
    assert!(validator.is_valid(&value));
    value["graphs"][0]["ops"][0]["note"] = json!("not a field");
    assert!(!validator.is_valid(&value));
    value["graphs"][0]["ops"][0] = op;
    value["wir_version"] = json!("0.2");
    assert!(!validator.is_valid(&value));
    value["wir_version"] = json!(wir::VERSION);
    value["graphs"][0]["ops"][0]["path"] = json!("a b");
    assert!(!validator.is_valid(&value));
}
