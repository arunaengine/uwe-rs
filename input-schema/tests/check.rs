//! The check of the values of a run against its schema: required inputs, types, enums, texts
//! of the command line, files and directories, and the groups of nf-schema.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use input_schema::{Problem, Severity, check, validate};
use serde_json::{Value as Json, json};
use workflow_graph::Inputs;

fn inputs(schema: Json, values: Json, base: &Path) -> Inputs {
    Inputs {
        schema,
        values,
        base: base.to_owned(),
        ..Inputs::default()
    }
}

fn messages(problems: &[Problem]) -> Vec<(Severity, &str)> {
    problems
        .iter()
        .map(|problem| (problem.severity, problem.message.as_str()))
        .collect()
}

#[test]
fn checks_value_constraints() {
    let schema = json!({
        "$schema": input_schema::DRAFT,
        "type": "object",
        "required": ["input", "outdir"],
        "properties": {
            "input": {"type": "string"},
            "outdir": {"type": "string"},
            "threads": {"type": "integer"},
            "aligner": {"type": "string", "enum": ["star", "hisat2"]}
        }
    });
    let values = json!({"outdir": "out", "threads": "many", "aligner": "bwa"});
    let mut problems = check(&inputs(schema, values, Path::new("/")), false);
    problems.sort_by(|a, b| a.input.cmp(&b.input));
    assert_eq!(
        messages(&problems),
        [
            (
                Severity::Error,
                "the input `aligner` must be one of `star` or `hisat2`, but it is `bwa`"
            ),
            (
                Severity::Error,
                "the input `input` is required, but has no value"
            ),
            (
                Severity::Error,
                "the input `threads` must be an integer, but it is the text `many`"
            ),
        ]
    );
}

#[test]
fn coerces_cli_values() {
    let schema = json!({
        "type": "object",
        "properties": {
            "threads": {"type": "integer"},
            "ratio": {"type": "number"},
            "skip": {"type": "boolean"},
            "either": {"anyOf": [{"type": "integer"}, {"type": "boolean"}]}
        }
    });
    let values = json!({"threads": "4", "ratio": "0.5", "skip": "True", "either": "7"});
    assert_eq!(check(&inputs(schema, values, Path::new("/")), false), []);
}

#[test]
fn returns_normalized_diagnostics() {
    let schema = json!({
        "type": "object",
        "required": ["missing/name~"],
        "properties": {
            "samples": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "count": {"type": "integer"},
                        "ratio": {"type": "number"},
                        "enabled": {"type": "boolean"}
                    }
                }
            },
            "a/b~c": {"type": "integer"},
            "defaulted": {"type": "integer", "default": 3}
        }
    });
    let values = json!({
        "samples": [
            {"count": "7", "ratio": "0.5", "enabled": "False"},
            {"count": "many"}
        ],
        "a/b~c": "wrong"
    });
    let details = validate(&inputs(schema, values, Path::new("/")), false);

    assert_eq!(details.values["samples"][0]["count"], json!(7));
    assert_eq!(details.values["samples"][0]["ratio"], json!(0.5));
    assert_eq!(details.values["samples"][0]["enabled"], json!(false));
    assert!(details.values.get("defaulted").is_none());
    let nested = details
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.instance_path == "/samples/1/count")
        .expect("the nested array error");
    assert_eq!(
        nested.schema_path,
        "/properties/samples/items/properties/count/type"
    );
    assert_eq!(
        (nested.keyword.as_str(), nested.code.as_str()),
        ("type", "schema.type")
    );
    let escaped = details
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.input == "a/b~c")
        .expect("the escaped property error");
    assert_eq!(escaped.instance_path, "/a~1b~0c");
    assert_eq!(escaped.schema_path, "/properties/a~1b~0c/type");
    let required = details
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.keyword == "required")
        .expect("the required error");
    assert_eq!(required.instance_path, "/missing~1name~0");
    assert_eq!(required.schema_path, "/required");
    assert_eq!(details.json()["values"], details.values);
}

#[test]
fn preserves_choice_branches() {
    let schema = json!({
        "properties": {
            "choice": {
                "oneOf": [
                    {"type": "integer"},
                    {"anyOf": [
                        {"type": "boolean"},
                        {"enum": ["yes"]}
                    ]}
                ]
            }
        }
    });
    let details = validate(
        &inputs(schema, json!({"choice": []}), Path::new("/")),
        false,
    );

    let choice = details.diagnostics.first().expect("the oneOf error");
    assert_eq!(
        (choice.keyword.as_str(), choice.code.as_str()),
        ("oneOf", "schema.oneOf")
    );
    assert_eq!(choice.instance_path, "/choice");
    assert_eq!(choice.schema_path, "/properties/choice/oneOf");
    assert_eq!(choice.branches.len(), 2);
    assert_eq!(choice.branches[0][0].keyword, "type");
    let nested = &choice.branches[1][0];
    assert_eq!(nested.keyword, "anyOf");
    assert_eq!(nested.branches.len(), 2);
    assert_eq!(nested.branches[0][0].keyword, "type");
    assert_eq!(nested.branches[1][0].keyword, "enum");
    assert_eq!(
        details.json()["diagnostics"][0]["branches"][1][0]["code"],
        "schema.anyOf"
    );
}

#[test]
fn files_and_directories() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(folder.path().join("sheet.csv"), "a,b\n").expect("a file");
    std::fs::create_dir(folder.path().join("data")).expect("a folder");
    let schema = json!({
        "type": "object",
        "properties": {
            "sheet": {"type": "string", "format": "file-path", "exists": true},
            "missing": {"type": "string", "format": "file-path", "exists": true},
            "folder": {"type": "string", "format": "file-path"},
            "outdir": {"type": "string", "format": "directory-path"},
            "remote": {"type": "string", "format": "file-path", "exists": true},
            "reads": {"type": "array", "items": {"type": "string", "format": "path", "exists": true}}
        }
    });
    let values = json!({
        "sheet": "sheet.csv",
        "missing": "gone.csv",
        "folder": "data",
        "outdir": "results",
        "remote": "https://example.org/a.csv",
        "reads": ["data", "sheet.csv", "b.fq"]
    });
    let given = inputs(schema, values, folder.path());
    let mut problems = check(&given, false);
    problems.sort_by(|a, b| a.input.cmp(&b.input));
    assert_eq!(
        messages(&problems),
        [
            (
                Severity::Error,
                "the input `folder` names `data`, which is a directory, not a file"
            ),
            (
                Severity::Error,
                "the input `missing` names the file `gone.csv`, which does not exist"
            ),
            (
                Severity::Error,
                "the input `reads[2]` names the file or directory `b.fq`, which does not exist"
            ),
        ]
    );
    let simulated = check(&given, true);
    assert!(
        simulated
            .iter()
            .filter(|problem| problem.message.contains("does not exist"))
            .all(|problem| problem.severity == Severity::Warning
                && problem.message.ends_with("; the simulated run assumes it"))
    );
    assert!(
        simulated
            .iter()
            .any(|problem| problem.severity == Severity::Error && problem.input == "folder")
    );
}

#[test]
fn warns_simulated_values() {
    let schema =
        json!({"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}});
    let mut given = inputs(schema, json!({}), Path::new("/"));
    given.assumes_values = true;
    let problems = check(&given, true);
    assert_eq!(
        messages(&problems),
        [(
            Severity::Warning,
            "the input `name` is required, but has no value; the simulated run makes one up"
        )]
    );
    assert_eq!(check(&given, false)[0].severity, Severity::Error);
}

#[test]
fn warns_advisory_failures() {
    let schema = json!({"type": "object", "required": ["name"], "properties": {"threads": {"type": "integer"}}});
    let mut given = inputs(schema, json!({"threads": "many"}), Path::new("/"));
    given.advisory = true;
    let problems = check(&given, false);
    assert_eq!(problems.len(), 2);
    assert!(
        problems
            .iter()
            .all(|problem| problem.severity == Severity::Warning)
    );
}

#[test]
fn counts_ignored_inputs() {
    let schema = json!({
        "type": "object",
        "required": ["data", "model"],
        "properties": {"data": {"type": "integer"}, "model": {"type": "integer"}}
    });
    let mut given = inputs(schema, json!({"data": "text"}), Path::new("/"));
    given.ignored = ["data", "model"].map(str::to_owned).into();
    assert_eq!(
        messages(&check(&given, false)),
        [(
            Severity::Error,
            "the input `model` is required, but has no value"
        )]
    );
}

#[test]
fn checks_schema_groups() {
    let schema = json!({
        "$defs": {"io": {"type": "object", "required": ["input"], "properties": {"input": {"type": "string"}, "max_cpus": {"type": "integer"}}}},
        "allOf": [{"$ref": "#/$defs/io"}]
    });
    let problems = check(
        &inputs(schema, json!({"max_cpus": "8"}), Path::new("/")),
        false,
    );
    assert_eq!(
        messages(&problems),
        [(
            Severity::Error,
            "the input `input` is required, but has no value"
        )]
    );
}

#[test]
fn explains_choice_failures() {
    let schema = json!({
        "type": "object",
        "anyOf": [
            {"required": ["accession"], "not": {"required": ["samplesheet"]}},
            {"required": ["samplesheet"]}
        ],
        "properties": {"accession": {"type": "string"}, "samplesheet": {"type": "string"}}
    });
    let problems = check(&inputs(schema, json!({}), Path::new("/")), false);
    assert_eq!(
        messages(&problems),
        [(
            Severity::Error,
            "the inputs fit none of the choices of the schema: the input `accession` is \
             required, but has no value; or the input `samplesheet` is required, but has no \
             value"
        )]
    );
}

#[test]
fn warns_missing_alternatives() {
    let folder = tempfile::tempdir().expect("a folder");
    for keyword in ["anyOf", "oneOf"] {
        let schema = json!({"properties": {"input": {keyword: [
            {"type": "string", "format": "file-path", "exists": true},
            {"type": "string", "format": "directory-path", "exists": true}
        ]}}});
        let given = inputs(schema.clone(), json!({"input": "absent"}), folder.path());
        assert_eq!(check(&given, true)[0].severity, Severity::Warning);
        assert_eq!(check(&given, false)[0].severity, Severity::Error);
        let wrong_type = inputs(schema, json!({"input": []}), folder.path());
        assert_eq!(check(&wrong_type, true)[0].severity, Severity::Error);
    }
    let schema = json!({"properties": {"input": {"anyOf": [
        {"type": "string", "format": "file-path", "exists": true},
        {"type": "integer"}
    ]}}});
    let given = inputs(schema, json!({"input": "absent"}), folder.path());
    assert_eq!(check(&given, true)[0].severity, Severity::Warning);
}

#[test]
fn preserves_numeric_integers() {
    for kind in ["number", "integer"] {
        for number in [
            json!(9007199254740993u64),
            json!(i64::MIN),
            json!(i64::MAX),
            json!(u64::MAX),
        ] {
            let inputs = inputs(
                json!({"type": kind}),
                json!(number.to_string()),
                Path::new("/"),
            );
            let validated = validate(&inputs, false);
            assert_eq!(validated.values, number);
            assert!(validated.diagnostics.is_empty());
        }
    }
}

#[test]
fn combines_required_declarations() {
    for schema in [
        json!({
            "allOf": [
                {"properties": {"x": {"type": "string"}}},
                {"required": ["x"]}
            ]
        }),
        json!({
            "$defs": {"group": {"properties": {"x": {"type": "string"}}}},
            "allOf": [{"$ref": "#/$defs/group"}],
            "required": ["x"]
        }),
        json!({
            "properties": {"x": {"type": "string"}},
            "allOf": [{"allOf": [{"required": ["x"]}, {"$ref": "#"}]}]
        }),
    ] {
        assert!(input_schema::text(&schema).contains("--x <text> (required)"));
        let inputs = inputs(schema, json!({}), Path::new("/"));
        assert_eq!(input_schema::Counts::of(&inputs, false).required, 1);
        assert!(
            check(&inputs, false)
                .iter()
                .any(|problem| problem.severity == Severity::Error)
        );
    }
}
