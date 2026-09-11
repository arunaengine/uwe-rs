//! nf-schema functions against the plugin nf-schema 2.5.1 with Nextflow 26.04.6; the fixtures
//! have the script that recorded the expected values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use std::cell::RefCell;
use std::rc::Rc;

use nextflow_evaluator::value::{Object, State, Value, Workflow};
use nextflow_plugins::{Context, call};

/// A host whose launch directory is the fixture directory.
struct Fixtures(String);

impl Host for Fixtures {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

fn directory() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/samplesheet")
        .to_string_lossy()
        .into_owned()
}

fn to_list(sheet: &str, schema: &str) -> Result<String, Error> {
    let directory = directory();
    to_list_in(Path::new(&directory), sheet, schema)
}

fn to_list_in(directory: &Path, sheet: &str, schema: &str) -> Result<String, Error> {
    let directory = directory.to_string_lossy().into_owned();
    let mut host = Fixtures(directory.clone());
    let config = Config::default();
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: &directory,
    };
    let arguments = vec![Value::string(sheet), Value::string(schema)];
    call("nf-schema", "samplesheetToList", arguments, &mut context).map(|rows| rows.to_string())
}

#[test]
fn referenced_rows_meta_paths_and_defaults_are_converted() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("sheet.csv"),
        "sample,fastq,group\nS1,reads.fq,\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("schema.json"),
        r##"{
          "$schema": "https://json-schema.org/draft/2020-12/schema",
          "type": "array",
          "items": {"$ref": "#/$defs/row"},
          "$defs": {
            "row": {
              "type": "object",
              "properties": {
                "sample": {"$ref": "#/$defs/sample"},
                "fastq": {"$ref": "#/$defs/path"},
                "group": {"$ref": "#/$defs/group"}
              }
            },
            "sample": {"type": "string", "meta": ["id"]},
            "path": {"type": "string", "format": "file-path"},
            "group": {"type": "integer", "default": 7}
          }
        }"##,
    )
    .unwrap();
    let rows = to_list_in(directory.path(), "sheet.csv", "schema.json").unwrap();
    assert_eq!(
        rows,
        format!("[[[id:S1], {}/reads.fq, 7]]", directory.path().display())
    );
}

#[test]
fn unsupported_or_broken_sample_references_fail_explicitly() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("sheet.csv"), "value\nx\n").unwrap();
    for (reference, definitions, expected) in [
        (
            "#/$defs/missing",
            "{}",
            "pointer segment `missing` does not exist",
        ),
        ("rows.json#/$defs/row", "{}", "external reference"),
        (
            "#/$defs/row",
            r##"{"row":{"$ref":"#/$defs/row"}}"##,
            "cyclic samplesheet schema reference",
        ),
    ] {
        let schema = format!(
            r##"{{"type":"array","items":{{"$ref":"{reference}"}},"$defs":{definitions}}}"##
        );
        std::fs::write(directory.path().join("schema.json"), schema).unwrap();
        let error = to_list_in(directory.path(), "sheet.csv", "schema.json")
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn structural_compositions_fail_and_literal_ids_remain_data() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("sheet.json"), r#"[{"sample":"S1"}]"#).unwrap();
    std::fs::write(
        directory.path().join("schema.json"),
        r#"{"type":"array","items":{"type":"object","allOf":[{"properties":{"sample":{"type":"string"}}}]}}"#,
    )
    .unwrap();
    let error = to_list_in(directory.path(), "sheet.json", "schema.json")
        .unwrap_err()
        .to_string();
    assert!(error.contains("`properties` under `allOf`"), "{error}");

    std::fs::write(directory.path().join("sheet.json"), r#"[{"$id":"row"}]"#).unwrap();
    std::fs::write(
        directory.path().join("schema.json"),
        r#"{
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "$id": {"type": "string"},
              "payload": {
                "type": "object",
                "properties": {"$id": {"type": "string"}},
                "default": {"$id": "data"}
              }
            }
          }
        }"#,
    )
    .unwrap();
    assert_eq!(
        to_list_in(directory.path(), "sheet.json", "schema.json").unwrap(),
        "[[row, [data]]]"
    );
}

#[test]
fn samplesheets_become_lists() {
    let dir = directory();
    assert_eq!(
        to_list("sheet.csv", "schema.json").unwrap(),
        format!(
            "[[[id:S1, group:1, grp:1, flag:true, note:hello], /data/a_1.fq.gz, /data/a_2.fq.gz, 1.5], \
             [[id:S2, group:[], grp:[], flag:false, note:[]], {dir}/b_1.fq.gz, [], []]]"
        )
    );
    assert_eq!(
        to_list("sheet2.csv", "schema2.json").unwrap(),
        format!("[[{dir}/a.fq, 7], [{dir}/b.fq, 3]]")
    );
    assert_eq!(
        to_list("sheet2.csv", "schema3.json").unwrap(),
        format!("[[{dir}/a.fq], [{dir}/b.fq]]")
    );
    assert_eq!(
        to_list("sheet4.csv", "schema4.json").unwrap(),
        "[[8000, 61.0, false], [10, [], unknown]]"
    );
    assert_eq!(
        to_list("sheet.tsv", "schema.json").unwrap(),
        format!("[[[id:S1, group:[], grp:[], flag:[], note:[]], {dir}/x.fq, [], []]]")
    );
    // Types come from the text, a `oneOf` with path formats gives a path, and a sheet
    // without header gives single values.
    let fixtures = dir.trim_end_matches("/samplesheet");
    assert_eq!(
        to_list("sheet5.csv", "schema5.json").unwrap(),
        format!(
            "[[[id:S1], 007, -0.0, {dir}/sheet.csv, {fixtures}/params], \
             [[id:S2], [], 1E+3, {dir}/sheet2.csv, {dir}/schema.json]]"
        )
    );
    assert_eq!(to_list("ids.csv", "schema6.json").unwrap(), "[SRR1, SRR22]");
}

#[test]
fn invalid_samplesheets_fail() {
    let error = to_list("bad.csv", "schema.json").unwrap_err().to_string();
    assert!(
        error.ends_with(
            "bad.csv:\n\n-> Entry 1: Error for field 'sample' (S 1): \"S 1\" does not match \
             regular expression [^\\S+$]\n-> Entry 1: Missing required field(s): fastq_1\n"
        ),
        "{error}"
    );
    let error = to_list("req.csv", "schema.json").unwrap_err().to_string();
    assert!(
        error.ends_with(
            "req.csv:\n\n-> Entry 1: Error for field 'group' (x): Value is [string] but should \
             be [integer]\n-> Entry 1: Missing required field(s): fastq_1, sample\n"
        ),
        "{error}"
    );
    let error = to_list("bad5.csv", "schema5.json").unwrap_err().to_string();
    assert!(
        error.ends_with(
            "bad5.csv:\n\n-> Entry 1: Error for field 'f' (../params): '../params' is not a \
             file, but a directory\n-> Entry 2: Error for field 'f' (missing.csv): the file or \
             directory 'missing.csv' does not exist\n"
        ),
        "{error}"
    );
}

#[test]
fn values_are_validated_against_a_schema() {
    let directory = directory();
    let mut host = Fixtures(directory.clone());
    let config = Config::default();
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: &directory,
    };
    let mut validate = |arguments: Vec<Value>| {
        call("nf-schema", "validate", arguments, &mut context).map(|errors| errors.to_string())
    };
    let input = |entries: &[(&str, Value)]| {
        Value::map(
            entries
                .iter()
                .map(|(key, value)| (Value::string(*key), value.clone())),
        )
    };
    let schema = || Value::string("object_schema.json");
    let good = input(&[("n", Value::integer(1))]);
    assert_eq!(validate(vec![good, schema()]).unwrap(), "[]");
    let bad = input(&[
        ("n", Value::string("a")),
        ("f", Value::string("missing.txt")),
    ]);
    let options = input(&[("exitOnError", Value::Boolean(false))]);
    assert_eq!(
        validate(vec![options, bad.clone(), schema()]).unwrap(),
        "[/f (missing.txt): the file or directory 'missing.txt' does not exist, \
         /n (a): Value is [string] but should be [integer]]"
    );
    let error = validate(vec![bad, schema()]).unwrap_err().to_string();
    assert!(
        error.ends_with("/n (a): Value is [string] but should be [integer]\n"),
        "{error}"
    );
}

/// The params of the probe: `nextflow.config` sets `input = 'x.csv'`, `outdir = null`,
/// `n = 5`, `flag = false` and `mode = 'a'`, and `main.nf` prints `paramsSummaryMap(workflow)`
/// and `paramsSummaryLog(workflow)`, then calls `validateParameters()`.
fn params(extra: &[(&str, &str)]) -> Value {
    let mut entries = vec![
        (Value::string("input"), Value::string("x.csv")),
        (Value::string("outdir"), Value::Null),
        (Value::string("n"), Value::integer(5)),
        (Value::string("flag"), Value::Boolean(false)),
        (Value::string("mode"), Value::string("a")),
    ];
    for (name, value) in extra {
        entries.push((Value::string(*name), Value::string(*value)));
    }
    Value::map(entries)
}

fn params_call(name: &str, params: Value, arguments: Vec<Value>) -> Result<String, Error> {
    params_call_in(&Config::default(), name, params, arguments)
}

/// Calls a function with the params of the probe and a config.
fn params_call_in(
    config: &Config,
    name: &str,
    params: Value,
    arguments: Vec<Value>,
) -> Result<String, Error> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/params")
        .to_string_lossy()
        .into_owned();
    let mut host = Fixtures(directory.clone());
    let mut context = Context {
        host: &mut host,
        config,
        params,
        project_dir: &directory,
    };
    call("nf-schema", name, arguments, &mut context).map(|value| value.to_string())
}

#[test]
fn parameters_are_validated_as_nf_schema_does() {
    let error = params_call(
        "validateParameters",
        params(&[("outdir", "res"), ("n", "7")]),
        vec![],
    );
    assert_eq!(
        error.unwrap_err().to_string(),
        "SchemaValidationException: The following invalid input values have been detected:\n\n\
         * --n (7): Value is [string] but should be [integer]\n\n"
    );
    let bad = params(&[("outdir", "res"), ("mode", "c"), ("input", "a b.txt")]);
    let error = params_call("validateParameters", bad, vec![])
        .unwrap_err()
        .to_string();
    assert!(
        error.ends_with(
            "* --input (a b.txt): \"a b.txt\" does not match regular expression [^\\S+\\.csv$]\n\
             * --mode (c): Expected any of [[a, b]]\n\n"
        ),
        "{error}"
    );
    let error = params_call("validateParameters", params(&[]), vec![])
        .unwrap_err()
        .to_string();
    assert!(
        error.ends_with("* Missing required parameter(s): outdir\n\n"),
        "{error}"
    );
    // As nf-schema's `cleanParameters`, values that Groovy counts as false are not checked,
    // so an empty text or list passes for a string and an empty `outdir` is missing.
    let falsy = |outdir: &str| {
        Value::map([
            (Value::string("input"), Value::string("")),
            (Value::string("outdir"), Value::string(outdir)),
            (Value::string("n"), Value::integer(0)),
            (Value::string("flag"), Value::Boolean(false)),
            (Value::string("mode"), Value::list(Vec::new())),
        ])
    };
    let error = params_call("validateParameters", falsy(""), vec![])
        .unwrap_err()
        .to_string();
    assert!(
        error.ends_with(":\n\n* Missing required parameter(s): outdir\n\n"),
        "{error}"
    );
    assert!(params_call("validateParameters", falsy("res"), vec![]).is_ok());
}

/// With `validation.lenientMode`, a `string` param also takes a number, as nf-schema's
/// `LenientTypeEvaluator` allows.
#[test]
fn lenient_mode_takes_numbers_for_strings() {
    let params = || {
        Value::map([
            (Value::string("outdir"), Value::integer(7)),
            (Value::string("n"), Value::integer(5)),
        ])
    };
    let error = params_call("validateParameters", params(), vec![]).unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with("* --outdir (7): Value is [integer] but should be [string]\n\n"),
        "{error}"
    );
    let directory = tempfile::tempdir().expect("a temporary directory");
    let file = directory.path().join("nextflow.config");
    std::fs::write(&file, "validation {\n    lenientMode = true\n}\n").expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
    };
    let mut host = Fixtures(String::new());
    let config = nextflow_config::load(&[file], &options, &mut host).expect("loads");
    assert!(params_call_in(&config, "validateParameters", params(), vec![]).is_ok());
}

#[test]
fn summaries_show_what_differs_from_the_defaults() {
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let properties = Value::map([
        text("runName", "magical_mahavira"),
        text("launchDir", "/tmp/nfschema2"),
        text("workDir", "/tmp/nfschema2/work"),
        text("projectDir", "/tmp/nfschema2"),
        text("userName", "beavis"),
        text("profile", "standard"),
        (
            Value::string("configFiles"),
            Value::list(vec![Value::string("/tmp/nfschema2/nextflow.config")]),
        ),
    ]);
    let workflow = Value::Object(Rc::new(RefCell::new(Object {
        class: "nextflow.script.WorkflowMetadata",
        state: State::Workflow(Workflow {
            properties,
            on_complete: Vec::new(),
            on_error: Vec::new(),
        }),
    })));
    let values = params(&[("outdir", "res"), ("n", "7")]);
    let map = params_call("paramsSummaryMap", values.clone(), vec![workflow.clone()]).unwrap();
    assert_eq!(
        map,
        "[Input/output options:[input:x.csv, outdir:res], Generic options:[n:7], \
         Core Nextflow options:[runName:magical_mahavira, launchDir:/tmp/nfschema2, \
         workDir:/tmp/nfschema2/work, projectDir:/tmp/nfschema2, userName:beavis, \
         profile:standard, configFiles:/tmp/nfschema2/nextflow.config]]"
    );
    let log = params_call("paramsSummaryLog", values, vec![workflow]).unwrap();
    let expected = [
        "Input/output options",
        "  input      : x.csv",
        "  outdir     : res",
        "Generic options",
        "  n          : 7",
        "Core Nextflow options",
        "  runName    : magical_mahavira",
        "  launchDir  : /tmp/nfschema2",
        "  workDir    : /tmp/nfschema2/work",
        "  projectDir : /tmp/nfschema2",
        "  userName   : beavis",
        "  profile    : standard",
        "  configFiles: /tmp/nfschema2/nextflow.config",
        "!! Only displaying parameters that differ from the pipeline defaults !!",
        "------------------------------------------------------",
    ];
    assert_eq!(log, expected.join("\n"));
}

#[test]
fn ignored_parameters_that_the_schema_knows_are_validated() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/params")
        .to_string_lossy()
        .into_owned();
    let config = Config::default();
    let ignored = ["outdir", "extra", "mode"].map(Value::string).to_vec();
    let ignored = Value::list(ignored);
    let path = ["validation".into(), "defaultIgnoreParams".into()];
    nextflow_config::tree::assign(&config.tree, &path, ignored).unwrap();
    let validate = |params: Value| {
        let mut host = Fixtures(directory.clone());
        let mut context = Context {
            host: &mut host,
            config: &config,
            params,
            project_dir: &directory,
        };
        call("nf-schema", "validateParameters", vec![], &mut context).map(|_| ())
    };
    assert!(validate(params(&[("outdir", "res"), ("extra", "x")])).is_ok());
    // nf-schema drops the errors that start with `* --<ignored param>`, like sarek's
    // `freebayes_filter` of the wrong type.
    assert!(validate(params(&[("outdir", "res"), ("mode", "c")])).is_ok());
    let error = validate(params(&[("extra", "x")])).unwrap_err().to_string();
    assert!(
        error.ends_with("* Missing required parameter(s): outdir\n\n"),
        "{error}"
    );
}

/// All params are checked, also when the groups of the schema are below `defs`, which
/// nf-schema does not take for groups, as Nextflow 26.04.6 runs nf-synteny.
#[test]
fn params_outside_the_groups_are_checked() {
    let options = Value::map([(
        Value::string("parameters_schema"),
        Value::string("schema_defs.json"),
    )]);
    assert!(params_call("validateParameters", params(&[]), vec![options]).is_ok());
}
