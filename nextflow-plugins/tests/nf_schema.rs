//! nf-schema functions against the plugin nf-schema 2.5.1 with Nextflow 26.04.6; the fixtures
//! have the script that recorded the expected values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
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
        to_list("sheet.tsv", "schema.json").unwrap(),
        format!("[[[id:S1, group:[], grp:[], flag:[], note:[]], {dir}/x.fq, [], []]]")
    );
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
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/params")
        .to_string_lossy()
        .into_owned();
    let mut host = Fixtures(directory.clone());
    let config = Config::default();
    let mut context = Context {
        host: &mut host,
        config: &config,
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
}

#[test]
fn summaries_show_what_differs_from_the_defaults() {
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let workflow = Value::map([
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
