//! The check of parameter values against the JSON Schema of a tool, with its conditions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jsonschema::Draft;
use serde_json::Value as Json;

use crate::finding::{Aspect, Finding, Fit};

/// Checks `values` against `schema`, a JSON Schema of draft 2020-12: one finding per problem,
/// none when they fit or the schema is null. A schema that does not compile gives unknown.
pub fn check(schema: &Json, values: &Json) -> Vec<Finding> {
    if schema.is_null() {
        return Vec::new();
    }
    let validator = match jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(schema)
    {
        Ok(validator) => validator,
        Err(error) => {
            let reason = format!("the parameter schema of the tool is not valid: {error}");
            return vec![Finding::new(Aspect::Parameter, Fit::Unknown, reason)];
        }
    };
    validator
        .iter_errors(values)
        .map(|error| {
            let reason = match error.instance_path().as_str() {
                "" => error.to_string(),
                at => format!("{error} (at {at})"),
            };
            Finding::new(Aspect::Parameter, Fit::DoesNotFit, reason)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn conditional_required() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {"enum": ["single", "paired"]},
                "fragment": {"type": "integer", "minimum": 1}
            },
            "if": {"properties": {"mode": {"const": "single"}}},
            "then": {"required": ["fragment"]},
            "dependentRequired": {"fragment": ["mode"]}
        });
        assert!(check(&schema, &json!({"mode": "paired"})).is_empty());
        let single = check(&schema, &json!({"mode": "single"}));
        assert_eq!(single.len(), 1, "{single:?}");
        assert!(single[0].reason.contains("fragment"), "{single:?}");
        let alone = check(&schema, &json!({"fragment": 0}));
        assert_eq!(alone.len(), 2, "{alone:?}");
        assert!(alone.iter().all(|finding| finding.fit == Fit::DoesNotFit));
    }

    #[test]
    fn broken_schema_unknown() {
        let findings = check(&json!({"type": "text"}), &json!({}));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].fit, Fit::Unknown);
        assert!(check(&Json::Null, &json!({"any": 1})).is_empty());
    }
}
