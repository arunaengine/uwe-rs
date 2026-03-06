//! Loading config files: profiles, includes, params and selectors, against the oracle.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_config::{Config, ConfigError, Options, load};
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn options(directory: &Path, profiles: &[&str], params: Value) -> Options {
    Options {
        profiles: Some(profiles.iter().map(|name| name.to_string()).collect()),
        params,
        project_dir: directory.to_string_lossy().into_owned(),
        launch_dir: directory.to_string_lossy().into_owned(),
    }
}

fn load_text(text: &str) -> Result<Config, ConfigError> {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("nextflow.config");
    std::fs::write(&file, text).unwrap();
    load(
        &[file],
        &options(directory.path(), &[], Value::map([])),
        &mut Quiet,
    )
}

#[test]
fn assignments_blocks_and_selectors() {
    let text = "process {\n    cpus = 2\n    withName: 'FOO' { memory = 1.GB }\n}\n\
                params.a.b = 1\nprocess.cpus = 4\n";
    let config = load_text(text).unwrap();
    let get = |path: &[&str]| config.get(path).unwrap().to_string();
    assert_eq!(get(&["process", "cpus"]), "4");
    assert_eq!(get(&["process", "withName:FOO", "memory"]), "1 GB");
    assert_eq!(get(&["params", "a", "b"]), "1");
}

#[test]
fn writing_below_a_value_is_an_error() {
    let error = load_text("process.ext = 'x'\nprocess.ext.args = 'y'\n").unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with("2:1: Cannot set `process.ext.args`, because `process.ext` is not a map"),
        "{error}"
    );
}
