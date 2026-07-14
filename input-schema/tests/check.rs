//! The check of the values of a run against its schema: required inputs, types, enums, texts
//! of the command line, files and directories, and the groups of nf-schema.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use input_schema::{Problem, Severity, check};
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
fn required_types_and_enums() {
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
    let values = json!({"input": null, "outdir": "out", "threads": "many", "aligner": "bwa"});
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
fn texts_from_the_command_line_read_as_numbers_and_booleans() {
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
fn made_up_values_are_warnings_in_simulated_runs() {
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
fn groups_of_nf_schema_are_checked() {
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
fn choices_say_what_each_one_lacks() {
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
