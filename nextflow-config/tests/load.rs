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

    fn secret(
        &mut self,
        name: &str,
    ) -> Result<Option<String>, nextflow_evaluator::interpreter::Error> {
        Ok((name == "TOKEN").then(|| "fixture".to_owned()))
    }
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
fn rejects_secret_values() {
    for read in [
        "secrets.TOKEN",
        "secrets['TOKEN']",
        "secrets.getProperty('TOKEN')",
        "secrets.getAt('TOKEN')",
    ] {
        let error = load_text(&format!("\nparams.token = {read}\n")).unwrap_err();
        assert!(error.unsupported());
        assert!(error.to_string().contains("process `secret` directive"));
        assert!(!error.to_string().contains("fixture"));
    }
}

#[test]
fn loads_included_secrets() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("nextflow.config");
    let included = directory.path().join("modules.config");
    std::fs::write(&main, "includeConfig 'modules.config'\n").unwrap();
    std::fs::write(
        &included,
        "\nprocess.maxForks = secrets.NCBI_API_KEY ? 8 : 3\n",
    )
    .unwrap();
    let config = load(
        &[main],
        &options(directory.path(), &[], Value::map([])),
        &mut Quiet,
    )
    .unwrap();
    assert_eq!(
        config.get(&["process", "maxForks"]).unwrap().to_string(),
        "3"
    );
}

#[test]
fn skips_inactive_secrets() {
    let config = load_text("params.enabled = false\nparams.token = params.enabled ? secrets.TOKEN : null\nprofiles {\n production { env.TOKEN = secrets.TOKEN }\n}\nprocess { withName: UNUSED { ext.args = { secrets.TOKEN } } }\n").unwrap();
    assert!(matches!(
        config.get(&["params", "token"]),
        Some(Value::Null)
    ));
    assert!(config.get(&["env", "TOKEN"]).is_none());
    assert!(
        config
            .get(&["process", "withName:UNUSED", "ext", "args"])
            .is_some()
    );
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("nextflow.config");
    std::fs::write(
        &main,
        "profiles { production { env.TOKEN = secrets.TOKEN } }\n",
    )
    .unwrap();
    let error = load(
        &[main],
        &options(directory.path(), &["production"], Value::map([])),
        &mut Quiet,
    )
    .unwrap_err();
    assert!(error.unsupported());
    assert!(!error.to_string().contains("fixture"));
}

#[test]
fn loads_config_forms() {
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
fn accepts_implicit_standard() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("nextflow.config");
    std::fs::write(&file, "params.value = 1\n").unwrap();
    let selected = |profiles: &[&str]| {
        load(
            std::slice::from_ref(&file),
            &options(directory.path(), profiles, Value::map([])),
            &mut Quiet,
        )
    };
    assert!(selected(&["standard"]).is_ok());
    assert!(selected(&["standard", "other"]).is_err());
    std::fs::write(&file, "profiles { standard { params.value = 2 } }\n").unwrap();
    let config = selected(&["standard"]).unwrap();
    assert_eq!(config.get(&["params", "value"]).unwrap().to_string(), "2");
}

/// A block replaces a null setting of its name, as Nextflow 26.04.6 and 21.10.3 do for
/// `params.x = null` and then `params { x { y = 1 } }`.
#[test]
fn replaces_null_settings() {
    let config =
        load_text("params.x = null\nparams {\n    x {\n        y = 1\n    }\n}\n").expect("loads");
    assert_eq!(
        nextflow_evaluator::value::tree::get(&config.tree, &["params", "x", "y"])
            .map(|value| value.to_string()),
        Some("1".into())
    );
}

#[test]
fn rejects_scalar_children() {
    let error = load_text("process.ext = 'x'\nprocess.ext.args = 'y'\n").unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with("2:1: Cannot set `process.ext.args`, because `process.ext` is not a map"),
        "{error}"
    );
}

#[test]
fn matches_flat_oracle() {
    let config = load_fixture(&["b", "a"], Value::map([])).unwrap();
    let expected = std::fs::read_to_string(fixture("profiles/oracle.txt")).unwrap();
    assert_eq!(config.flat(), expected);
}

#[test]
fn scopes_parameter_types() {
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
fn rejects_unknown_profiles() {
    let error = load_fixture(&["c"], Value::map([])).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Unknown configuration profile: 'c'\n\nAvailable config profiles: a, b"
    );
}

#[test]
fn orders_process_selectors() {
    let text = "process {\n    cpus = 1\n    ext.dflt = 'd'\n\
                withLabel: 'big' { cpus = 4 }\n    withLabel: '!big' { memory = 1.GB }\n\
                withName: 'OUTER:SUB:BAR' { ext.args = 'full' }\n\
                withName: 'FOO|BAR' { ext.args = 'foo' }\n    withName: BAR { ext.args = 'bar' }\n\
                withName: 'UNTAR_*' { ext.args = 'untar' }\n}\n";
    let config = load_text(text).unwrap();
    let show = |settings: &[(std::rc::Rc<str>, Value)]| {
        let items: Vec<String> = settings
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        items.join(" ")
    };
    let names = ["FOO", "BAR", "OUTER:SUB:BAR"];
    let settings = config.process_settings(&names, &[]).unwrap();
    assert_eq!(
        show(&settings.selected),
        "memory=1 GB ext=[args:foo] ext=[args:foo] ext=[args:bar] ext=[args:full]"
    );
    assert_eq!(show(&settings.defaults), "cpus=1 ext=[dflt:d]");
    let settings = config
        .process_settings(&["UNTAR_FOO"], &["big".to_owned()])
        .unwrap();
    assert_eq!(show(&settings.selected), "cpus=4");
}

/// The maps of params, also those in lists, are `nextflow.config.ConfigMap`, as Nextflow
/// 26.04.6 gives them to scripts; old nf-core schema checks tell them from `LinkedHashMap`.
#[test]
fn converts_parameter_maps() {
    let config = load_text("params.m = [a: [b: 1], l: [[c: 1]]]\n").expect("loads");
    let class = |path: &[&str]| {
        nextflow_evaluator::value::tree::get(&config.tree, path)
            .map(|value| value.java_class_name())
    };
    assert_eq!(class(&["params", "m"]), Some("nextflow.config.ConfigMap"));
    assert_eq!(
        class(&["params", "m", "a"]),
        Some("nextflow.config.ConfigMap")
    );
    let Some(Value::List(list)) =
        nextflow_evaluator::value::tree::get(&config.tree, &["params", "m", "l"])
    else {
        panic!("a list");
    };
    assert_eq!(
        list.borrow().items[0].java_class_name(),
        "nextflow.config.ConfigMap"
    );
}
