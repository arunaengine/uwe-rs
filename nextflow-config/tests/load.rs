//! Loading config files: profiles, includes, params and selectors, against the oracle.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_config::{Config, ConfigError, Options, load};
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn options(directory: &Path, profiles: &[&str], params: Value) -> Options {
    Options {
        profiles: Some(profiles.iter().map(|name| name.to_string()).collect()),
        params,
        project_dir: directory.to_string_lossy().into_owned(),
        launch_dir: directory.to_string_lossy().into_owned(),
    }
}

fn load_fixture(profiles: &[&str], params: Value) -> Result<Config, ConfigError> {
    let directory = fixture("profiles");
    let files = [directory.join("nextflow.config")];
    load(&files, &options(&directory, profiles, params), &mut Quiet)
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
    assert_eq!(get(&["env"]), "[:]");
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

#[test]
fn command_line_params_take_the_config_type_inside_the_config_only() {
    let params = Value::map([
        (Value::string("n"), Value::string("7")),
        (Value::string("flag"), Value::string("false")),
    ]);
    let config = load_fixture(&["a"], params).unwrap();
    let get = |name: &str| config.get(&["params", name]).unwrap();
    assert_eq!(get("is_int").to_string(), "true");
    // At run time, command-line values are the strings given.
    assert!(matches!(get("n"), Value::String(text) if &*text == "7"));
    // `params.flag` was true while the config ran, so it included the missing file.
    let error = load_fixture(
        &["a"],
        Value::map([(Value::string("flag"), Value::string("TRUE"))]),
    );
    assert!(error.unwrap_err().to_string().contains("missing.config"));
}

#[test]
fn unknown_profiles_are_errors() {
    let error = load_fixture(&["c"], Value::map([])).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Unknown configuration profile: 'c'\n\nAvailable config profiles: a, b"
    );
}
