//! nf-versions' commands against nf-versions 0.8.0 with Nextflow 26.04.6.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_plugins::{Context, call};

struct Silent;

impl Host for Silent {
    fn print(&mut self, _: &str) {}
}

/// The text a function returns, or the message of its exception.
fn text(name: &str, arguments: Vec<Value>) -> String {
    let config = Config::default();
    let mut host = Silent;
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: "/",
        cli_params: &[],
        workflow: Value::Null,
    };
    match call("nf-versions", name, arguments, &mut context) {
        Ok(value) => value.to_string(),
        Err(Error::Thrown { exception, .. }) => exception.to_string(),
        Err(other) => panic!("{other}"),
    }
}

#[test]
fn returns_recorded_commands() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/nf_versions/functions.json"
    );
    let recorded: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("reads")).expect("JSON");
    assert_eq!(recorded.len(), 30);
    for (call, expected) in recorded {
        let (name, arguments) = match call.split_once('(') {
            Some((name, argument)) => {
                let argument = argument.trim_end_matches(')');
                (name.to_owned(), vec![Value::string(argument)])
            }
            None => (call.clone(), Vec::new()),
        };
        assert_eq!(
            text(&name, arguments),
            expected.as_str().expect("text"),
            "{call}"
        );
    }
}

#[test]
fn checks_package_names() {
    for name in ["bad name", "a;b", ""] {
        assert_eq!(
            text("pyPackageVersion", vec![Value::string(name)]),
            format!(
                "IllegalArgumentException: Invalid Python package name '{name}': only letters, \
                 digits, hyphens, underscores, and dots are allowed (PEP 508)."
            )
        );
        assert_eq!(
            text("rLibraryVersion", vec![Value::string(name)]),
            format!(
                "IllegalArgumentException: Invalid R library name '{name}': only letters, \
                 digits, hyphens, underscores, and dots are allowed."
            )
        );
    }
    assert_eq!(
        text("pyPackageVersion", vec![Value::string("ok.name-1_x")]),
        "\necho 'ok.name-1_x: \"'$( python3 -c 'import sys; from importlib.metadata import \
         version; print(version(sys.argv[1]))' ok.name-1_x 2>/dev/null )'\"'\n"
    );
}
