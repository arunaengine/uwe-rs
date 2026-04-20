//! nf-validation functions against the plugin nf-validation 1.1.3 with Nextflow 26.04.6 and
//! its legacy parser; `fixtures/nf_validation/record.sh` recorded the expected values.
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
