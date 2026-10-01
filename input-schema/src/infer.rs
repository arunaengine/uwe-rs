//! Infers input schemas from default values and existing local paths.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json, json};

use crate::{DIRECTORY_PATH, DRAFT, FILE_PATH, is_remote};

/// The schema of inputs with these defaults: an object with one property per input, which
/// has the type of its default and the default itself. No input is required, as a default
/// stands for each. Relative paths start at `base`.
pub fn infer(defaults: &Map<String, Json>, base: &Path) -> Json {
    let properties: Map<String, Json> = defaults
        .iter()
        .map(|(name, value)| {
            let mut schema = value_schema(value, base);
            if !value.is_null()
                && let Json::Object(schema) = &mut schema
            {
                schema.insert("default".into(), value.clone());
            }
            (name.clone(), schema)
        })
        .collect();
    json!({"$schema": DRAFT, "type": "object", "properties": properties})
}

/// Infers JSON types, homogeneous array items, and object properties; null has no type.
/// Existing files become required file paths; existing directories may be made by the run.
pub fn value_schema(value: &Json, base: &Path) -> Json {
    match value {
        Json::Null => json!({}),
        Json::Bool(_) => json!({"type": "boolean"}),
        Json::Number(number) if number.is_i64() || number.is_u64() => json!({"type": "integer"}),
        Json::Number(_) => json!({"type": "number"}),
        Json::String(text) => text_schema(text, base),
        Json::Array(items) => {
            let mut schema = json!({"type": "array"});
            if let Some(first) = items.first()
                && items.iter().all(|item| same_type(item, first))
            {
                schema["items"] = value_schema(first, base);
            }
            schema
        }
        Json::Object(entries) => {
            let properties: Map<String, Json> = entries
                .iter()
                .map(|(name, value)| (name.clone(), value_schema(value, base)))
                .collect();
            json!({"type": "object", "properties": properties})
        }
    }
}

fn text_schema(text: &str, base: &Path) -> Json {
    let mut schema = json!({"type": "string"});
    if text.is_empty() || text.contains('\n') || is_remote(text) {
        return schema;
    }
    match std::fs::metadata(base.join(text)) {
        Ok(metadata) if metadata.is_dir() => schema["format"] = json!(DIRECTORY_PATH),
        Ok(_) => {
            schema["format"] = json!(FILE_PATH);
            schema["exists"] = json!(true);
        }
        Err(_) => {}
    }
    schema
}

fn same_type(a: &Json, b: &Json) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_from_defaults() {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(folder.path().join("samples.tsv"), "sample\n").expect("a file");
        std::fs::create_dir(folder.path().join("results")).expect("a folder");
        let defaults = json!({
            "samples": "samples.tsv",
            "outdir": "results",
            "genome": "GRCh38",
            "threads": 4,
            "ratio": 0.5,
            "trim": true,
            "input": null,
            "lanes": [1, 2],
            "reference": {"fasta": "https://example.org/g.fa"}
        });
        let Json::Object(defaults) = defaults else {
            unreachable!()
        };
        let schema = infer(&defaults, folder.path());
        assert_eq!(
            schema,
            json!({
                "$schema": DRAFT,
                "type": "object",
                "properties": {
                    "samples": {"type": "string", "format": "file-path", "exists": true, "default": "samples.tsv"},
                    "outdir": {"type": "string", "format": "directory-path", "default": "results"},
                    "genome": {"type": "string", "default": "GRCh38"},
                    "threads": {"type": "integer", "default": 4},
                    "ratio": {"type": "number", "default": 0.5},
                    "trim": {"type": "boolean", "default": true},
                    "input": {},
                    "lanes": {"type": "array", "items": {"type": "integer"}, "default": [1, 2]},
                    "reference": {
                        "type": "object",
                        "properties": {"fasta": {"type": "string"}},
                        "default": {"fasta": "https://example.org/g.fa"}
                    }
                }
            })
        );
        assert_eq!(crate::valid(&schema), Ok(()));
    }
}
