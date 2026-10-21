//! Typed JSON validation preserves submitted values without CLI coercion.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use input_schema::{Severity, validate, validate_json};
use serde_json::{Value as Json, json};
use workflow_graph::Inputs;

fn inputs(schema: Json, values: Json) -> Inputs {
    Inputs {
        schema,
        values,
        ..Inputs::default()
    }
}

#[test]
fn rejects_typed_strings() {
    let given = inputs(
        json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer"},
                "ratio": {"type": "number"},
                "enabled": {"type": "boolean"}
            }
        }),
        json!({"count": "7", "ratio": "0.5", "enabled": "true"}),
    );
    let submitted = given.values.clone();
    let typed = validate_json(&given, false);

    assert_eq!(typed.values, submitted);
    assert_eq!(typed.diagnostics.len(), 3);
    assert!(
        typed
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "schema.type")
    );

    let cli = validate(&given, false);
    assert!(cli.diagnostics.is_empty());
    assert_eq!(
        cli.values,
        json!({"count": 7, "ratio": 0.5, "enabled": true})
    );
}

#[test]
fn preserves_union_diagnostics() {
    let given = inputs(
        json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {"anyOf": [{"type": "integer"}, {"type": "boolean"}]}
                }
            }
        }),
        json!({"items": ["7", "false"]}),
    );
    let submitted = given.values.clone();
    let validation = validate_json(&given, false);

    assert_eq!(validation.values, submitted);
    assert_eq!(validation.diagnostics.len(), 2);
    for (index, diagnostic) in validation.diagnostics.iter().enumerate() {
        assert_eq!(diagnostic.code, "schema.anyOf");
        assert_eq!(diagnostic.instance_path, format!("/items/{index}"));
        assert_eq!(diagnostic.branches.len(), 2);
        assert_eq!(diagnostic.branches[0][0].code, "schema.type");
        assert_eq!(diagnostic.branches[1][0].code, "schema.type");
    }
}

#[test]
fn preserves_typed_values() {
    let given = inputs(
        json!({
            "type": "object",
            "required": ["minimum", "maximum"],
            "properties": {
                "minimum": {"type": "integer"},
                "maximum": {"type": "integer"},
                "defaulted": {"type": "integer", "default": 3}
            }
        }),
        json!({"minimum": i64::MIN, "maximum": i64::MAX}),
    );
    let validation = validate_json(&given, false);

    assert!(validation.diagnostics.is_empty());
    assert_eq!(validation.values["minimum"], json!(i64::MIN));
    assert_eq!(validation.values["maximum"], json!(i64::MAX));
    assert!(validation.values.get("defaulted").is_none());
}

#[test]
fn preserves_typed_diagnostics() {
    let given = inputs(
        json!({
            "type": "object",
            "required": ["required/name~"],
            "properties": {
                "choice": {"oneOf": [{"type": "integer"}, {"type": "boolean"}]},
                "defaulted": {"type": "string", "default": "annotation"}
            }
        }),
        json!({"choice": []}),
    );
    let validation = validate_json(&given, false);
    let required = validation
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "schema.required")
        .expect("required diagnostic");
    let choice = validation
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "schema.oneOf")
        .expect("oneOf diagnostic");

    assert_eq!(required.instance_path, "/required~1name~0");
    assert_eq!(required.severity, Severity::Error);
    assert_eq!(choice.instance_path, "/choice");
    assert_eq!(choice.branches.len(), 2);
    assert_eq!(
        choice.branches[0][0].schema_path,
        "/properties/choice/oneOf/0/type"
    );
    assert_eq!(
        choice.branches[1][0].schema_path,
        "/properties/choice/oneOf/1/type"
    );
    assert!(validation.values.get("defaulted").is_none());
}
