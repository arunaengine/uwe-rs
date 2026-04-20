//! nf-validation functions against the plugin nf-validation 1.1.3 with Nextflow 26.04.6 and
//! its legacy parser; `fixtures/nf_validation/record.sh` recorded the expected values.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
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
        .join("tests/fixtures/nf_validation")
        .to_string_lossy()
        .into_owned()
}

/// The params of `nextflow.config`, with `extra` from the command line over them.
fn params(extra: &[(&str, Value)]) -> Value {
    let text = |value: &str| Value::string(value);
    let mut entries = vec![
        (text("input"), text("sheet.csv")),
        (text("outdir"), text("res")),
        (text("n"), Value::integer(5)),
        (text("mode"), text("a")),
        (text("flag"), Value::Boolean(false)),
        (text("unknown_one"), text("u")),
    ];
    for (name, value) in extra {
        entries.push((text(name), value.clone()));
    }
    Value::map(entries)
}

fn run(name: &str, params: Value, arguments: Vec<Value>) -> Result<String, Error> {
    let directory = directory();
    let mut host = Fixtures(directory.clone());
    let config = Config::default();
    let mut context = Context {
        host: &mut host,
        config: &config,
        params,
        project_dir: &directory,
    };
    call("nf-validation", name, arguments, &mut context).map(|value| value.to_string())
}

#[test]
fn parameters_are_validated_as_nf_validation_does() {
    assert!(run("validateParameters", params(&[]), vec![]).is_ok());
    let bad = params(&[
        ("n", Value::string("x")),
        ("mode", Value::string("c")),
        ("outdir", Value::Boolean(true)),
    ]);
    let error = run("validateParameters", bad, vec![])
        .unwrap_err()
        .to_string();
    let (head, lines) = error.split_once("\n\n").unwrap();
    assert_eq!(
        head,
        "SchemaValidationException: The following invalid input values have been detected:"
    );
    // Everit gives the errors in an order of its own.
    let mut lines: Vec<&str> = lines.lines().filter(|line| !line.is_empty()).collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "* --mode: 'c' is not a valid choice (Available choices: a, b)",
            "* --n: expected type: Integer, found: String (x)",
            "* --outdir: expected type: String, found: Boolean (true)",
        ]
    );
}

#[test]
fn summaries_put_the_core_options_first() {
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let properties = Value::map([
        text("runName", "thirsty_knuth"),
        text("launchDir", "/tmp/nf-agent/probe-nfv"),
        text("workDir", "/tmp/nf-agent/probe-nfv/work"),
        text("projectDir", "/tmp/nf-agent/probe-nfv"),
        text("userName", "beavis"),
        text("profile", "standard"),
        (Value::string("configFiles"), Value::list(Vec::new())),
    ]);
    let workflow = Value::Object(Rc::new(RefCell::new(Object {
        class: "nextflow.script.WorkflowMetadata",
        state: State::Workflow(Workflow {
            properties,
            on_complete: Vec::new(),
            on_error: Vec::new(),
        }),
    })));
    let map = run("paramsSummaryMap", params(&[]), vec![workflow.clone()]).unwrap();
    assert_eq!(
        map,
        "[Core Nextflow options:[runName:thirsty_knuth, launchDir:/tmp/nf-agent/probe-nfv, \
         workDir:/tmp/nf-agent/probe-nfv/work, projectDir:/tmp/nf-agent/probe-nfv, \
         userName:beavis, profile:standard, configFiles:], Input/output options:[input:sheet.csv, \
         outdir:res], Generic options:[:]]"
    );
    let log = run("paramsSummaryLog", params(&[]), vec![workflow]).unwrap();
    let expected = [
        "Core Nextflow options",
        "  runName    : thirsty_knuth",
        "  launchDir  : /tmp/nf-agent/probe-nfv",
        "  workDir    : /tmp/nf-agent/probe-nfv/work",
        "  projectDir : /tmp/nf-agent/probe-nfv",
        "  userName   : beavis",
        "  profile    : standard",
        "  configFiles: ",
        "",
        "Input/output options",
        "  input      : sheet.csv",
        "  outdir     : res",
        "",
        "!! Only displaying parameters that differ from the pipeline defaults !!",
        "------------------------------------------------------",
    ];
    assert_eq!(log, expected.join("\n"));
}

#[test]
fn samplesheets_become_rows_with_meta_maps() {
    let dir = directory();
    let rows = run("fromSamplesheet", params(&[]), vec![Value::string("input")]).unwrap();
    assert_eq!(
        rows,
        format!(
            "[[[id:S1, reps:2], {dir}/a_1.fq, {dir}/a_2.fq, auto, 1.5, true], \
             [[id:S2, reps:1], {dir}/b_1.fq, [], rev, 3, false]]"
        )
    );
    let missing = params(&[("input", Value::string("missing.csv"))]);
    let error = run("validateParameters", missing, vec![])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("* --input: the file or directory 'missing.csv' does not exist."),
        "{error}"
    );
}
