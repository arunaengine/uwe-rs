//! Whether a schema is valid JSON Schema of draft 2020-12.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jsonschema::Draft;
use serde_json::Value as Json;

/// Checks that a schema fits the meta-schema of draft 2020-12, and that a validator can be
/// made of it: its references resolve and its patterns compile. The error says what is wrong
/// and where.
pub fn valid(schema: &Json) -> Result<(), String> {
    if let Err(error) = jsonschema::draft202012::meta::validate(schema) {
        return Err(match error.instance_path().as_str() {
            "" => error.to_string(),
            at => format!("{error} (at {at})"),
        });
    }
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(schema)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn invalid_schemas() {
        assert!(valid(&json!({"type": "text"})).is_err());
        assert!(valid(&json!({"$ref": "#/$defs/missing"})).is_err());
        assert!(valid(&json!({"type": "string", "pattern": "(unclosed"})).is_err());
        assert_eq!(
            valid(&json!({"type": "string", "format": "file-path"})),
            Ok(())
        );
    }
}
