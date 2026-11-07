//! Loads, verifies and round-trips the examples, and checks them against the schema.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use jsonschema::{Draft, Validator};
use serde_json::Value as Json;

const EXAMPLES: [&str; 2] = ["two-tools.json", "native-cwl.json"];

fn example(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn validator() -> Validator {
    let schema: Json = serde_json::from_str(wir::SCHEMA).expect("the schema is JSON");
    jsonschema::draft202012::meta::validate(&schema).expect("the schema fits draft 2020-12");
    (jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema))
    .expect("a validator")
}

#[test]
fn examples_verify() {
    for name in EXAMPLES {
        let document = wir::parse(&example(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(wir::verify(&document), Ok(()), "{name}");
    }
}

#[test]
fn examples_round_trip() {
    for name in EXAMPLES {
        let text = example(name);
        let original: Json = serde_json::from_str(&text).expect("an example is JSON");
        let document = wir::parse(&text).expect("an example parses");
        let written = serde_json::to_string_pretty(&document).expect("a document serializes");
        assert_eq!(
            serde_json::from_str::<Json>(&written).expect("JSON"),
            original,
            "{name}"
        );
        assert_eq!(
            wir::parse(&written).expect("parses again"),
            document,
            "{name}"
        );
    }
}

#[test]
fn examples_fit_schema() {
    let validator = validator();
    for name in EXAMPLES {
        let value: Json = serde_json::from_str(&example(name)).expect("an example is JSON");
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
}
