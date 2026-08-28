//! Config files of legacy syntax, which strict syntax rejects and iunctus runs with the
//! meaning of the legacy parser (decision 14).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_config::{Config, ConfigError, Options, load};
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::value::Value;

/// Keeps what config files print to `System.err`, and knows one environment variable.
#[derive(Default)]
struct Recorder {
    errors: String,
}

impl Host for Recorder {
    fn print(&mut self, _: &str) {}

    fn environment(&self, name: &str) -> Option<String> {
        (name == "IUNCTUS_TEST").then(|| "from the environment".into())
    }

    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        arguments: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        if let ("System.err", "println", [message]) = (namespace, name, arguments.as_slice()) {
            self.errors.push_str(&format!("{message}\n"));
            return Ok(Some(Value::Null));
        }
        Ok(None)
    }
}

/// Loads `nextflow.config` from files written into a new directory, with what it printed to
/// `System.err`.
fn load_files(files: &[(&str, &str)]) -> (Result<Config, ConfigError>, String) {
    let directory = tempfile::tempdir().unwrap();
    for (name, text) in files {
        std::fs::write(directory.path().join(name), text).unwrap();
    }
    let options = Options {
        profiles: None,
        params: Value::map([]),
        project_dir: directory.path().to_string_lossy().into_owned(),
        launch_dir: directory.path().to_string_lossy().into_owned(),
    };
    let mut host = Recorder::default();
    let files = [Path::new(directory.path()).join("nextflow.config")];
    let config = load(&files, &options, &mut host);
    (config, host.errors)
}

fn load_text(text: &str) -> Config {
    let (config, _) = load_files(&[("nextflow.config", text)]);
    config.unwrap()
}

fn get(config: &Config, path: &[&str]) -> String {
    match config.get(path) {
        Some(value) => value.to_string(),
        None => panic!("no setting {path:?} in:\n{}", config.flat()),
    }
}

#[test]
fn include_cycles_fail_and_repeated_includes_still_run() {
    for files in [
        vec![("nextflow.config", "includeConfig './nextflow.config'\n")],
        vec![
            ("nextflow.config", "includeConfig 'other.config'\n"),
            ("other.config", "includeConfig 'nextflow.config'\n"),
        ],
    ] {
        let error = load_files(&files).0.unwrap_err().to_string();
        assert!(error.contains("Config include cycle:"), "{error}");
        assert!(error.contains("nextflow.config"), "{error}");
    }
    let (config, _) = load_files(&[
        (
            "nextflow.config",
            "params.n = 0\nincludeConfig 'other.config'\nincludeConfig 'other.config'\n",
        ),
        ("other.config", "params.n = params.n + 1\n"),
    ]);
    assert_eq!(get(&config.unwrap(), &["params", "n"]), "2");
}

/// Calls a closure of the config without arguments, with the final params.
fn call(config: &Config, path: &[&str]) -> String {
    let Some(Value::Closure(closure)) = config.get(path) else {
        panic!("{path:?} is no closure");
    };
    let mut host = Recorder::default();
    let mut interpreter = Interpreter::new(&mut host);
    interpreter.set_global("params", config.params());
    interpreter
        .call_closure(&closure, Vec::new())
        .unwrap()
        .to_string()
}

#[test]
fn try_catches_a_failed_include() {
    let text = "params.base = 'missing'
try {
    includeConfig \"${params.base}/custom.config\"
} catch (Exception e) {
    System.err.println(\"WARNING: Could not load ${params.base}/custom.config\")
}
try {
    includeConfig 'missing.config'
} catch (IOException e) {
    params.caught = e instanceof java.nio.file.NoSuchFileException
}
params.after = 1
";
    let (config, errors) = load_files(&[("nextflow.config", text)]);
    let config = config.unwrap();
    assert_eq!(errors, "WARNING: Could not load missing/custom.config\n");
    assert_eq!(get(&config, &["params", "caught"]), "true");
    assert_eq!(get(&config, &["params", "after"]), "1");
}

#[test]
fn try_without_a_matching_catch_fails() {
    let text =
        "try {\n    includeConfig 'missing.config'\n} catch (IllegalStateException e) {\n}\n";
    let (config, _) = load_files(&[("nextflow.config", text)]);
    let error = config.unwrap_err().to_string();
    assert!(error.contains("missing.config"), "{error}");
}

#[test]
fn try_runs_an_include_that_works() {
    let text = "try {\n    includeConfig 'custom.config'\n} catch (Exception e) {\n    \
                params.caught = true\n}\n";
    let (config, _) = load_files(&[
        ("nextflow.config", text),
        ("custom.config", "params.custom = 'yes'\n"),
    ]);
    let config = config.unwrap();
    assert_eq!(get(&config, &["params", "custom"]), "yes");
    assert!(config.get(&["params", "caught"]).is_none());
}

#[test]
fn if_statements_choose_settings() {
    let config = load_text(
        "params.gpu = true
params.genome = null
if (params.genome == null) {
    params.genomes = [:]
} else {
    includeConfig 'missing.config'
}
process {
    if (params.gpu) {
        withName: 'FOO' {
            accelerator = 1
        }
    } else if (params.big)
        cpus = 8
    if (!params.gpu) {
        memory = 1.GB
    } else {
        time = 1.h
    }
}
",
    );
    assert_eq!(get(&config, &["params", "genomes"]), "[:]");
    assert_eq!(
        get(&config, &["process", "withName:FOO", "accelerator"]),
        "1"
    );
    assert_eq!(get(&config, &["process", "time"]), "1h");
    assert!(config.get(&["process", "cpus"]).is_none());
    assert!(config.get(&["process", "memory"]).is_none());
}

#[test]
fn selectors_at_the_top_level() {
    let config = load_text(
        "params.align = true
if (params.align) {
    withName: 'ALIGN' {
        cpus = 4
    }
}
process {
    withName: 'FOO' {
        saveAs: { filename -> filename }
        cpus = 2
    }
}
",
    );
    assert_eq!(get(&config, &["withName:ALIGN", "cpus"]), "4");
    assert_eq!(get(&config, &["process", "withName:FOO", "cpus"]), "2");
    let settings = config.process_settings(&["ALIGN"], &[]).unwrap();
    assert!(settings.selected.is_empty());
}

#[test]
fn local_variables() {
    let config = load_text(
        "def stamp = 'today'
timeline {
    file = \"timeline_${stamp}.html\"
}
process {
    def publish = [path: 'out']
    withName: FOO {
        publishDir = publish
        ext.args = { \"--date ${stamp}\" }
    }
}
def (first, second) = ['a', 'b']
stamp = 'tomorrow'
params.names = \"${first}${second}_${stamp}\"
",
    );
    assert_eq!(get(&config, &["timeline", "file"]), "timeline_today.html");
    let publish = get(&config, &["process", "withName:FOO", "publishDir"]);
    assert_eq!(publish, "[path:out]");
    assert_eq!(
        call(&config, &["process", "withName:FOO", "ext", "args"]),
        "--date today"
    );
    assert_eq!(get(&config, &["params", "names"]), "ab_tomorrow");
    let config = load_text(
        "def twice = { value -> value * 2 }
params.four = twice(2)
process.ext.args = {
    def half = { value -> value / 2 }
    \"${half(8)} ${twice(3)}\"
}
process.ext.prefix = { foo(1) }
",
    );
    assert_eq!(get(&config, &["params", "four"]), "4");
    assert_eq!(call(&config, &["process", "ext", "args"]), "4 6");
    // An unknown function in a config closure fails only when the closure runs.
    let Some(Value::Closure(prefix)) = config.get(&["process", "ext", "prefix"]) else {
        panic!("a closure");
    };
    let mut host = Recorder::default();
    let error = Interpreter::new(&mut host).call_closure(&prefix, Vec::new());
    let error = error.unwrap_err().to_string();
    assert!(error.starts_with("MissingMethodException"), "{error}");
    // A variable belongs to its block.
    let (config, _) = load_files(&[(
        "nextflow.config",
        "process {\n    def x = 1\n}\nparams.x = x\n",
    )]);
    let error = config.unwrap_err().to_string();
    assert!(error.ends_with("4:12: `x` is not defined"), "{error}");
}

#[test]
fn earlier_settings_by_name() {
    let (config, _) = load_files(&[
        (
            "nextflow.config",
            "params.base = 'https://example.org/'
manifest {
    version = '1.0'
}
validation {
    help {
        text = \"version ${manifest.version}\"
    }
}
env.FROM = IUNCTUS_TEST
includeConfig 'test.config'
",
        ),
        (
            "test.config",
            "params {\n    input = base + 'samplesheet.csv'\n}\n",
        ),
    ]);
    let config = config.unwrap();
    assert_eq!(get(&config, &["validation", "help", "text"]), "version 1.0");
    assert_eq!(get(&config, &["env", "FROM"]), "from the environment");
    let input = get(&config, &["params", "input"]);
    assert_eq!(input, "https://example.org/samplesheet.csv");
    // A name that is neither a setting nor an environment variable stays an error.
    let (config, _) = load_files(&[("nextflow.config", "params.x = manifest.name\n")]);
    let error = config.unwrap_err().to_string();
    assert!(
        error.ends_with("1:12: `manifest` is not defined"),
        "{error}"
    );
}

#[test]
fn functions_of_the_file_and_the_including_files() {
    let (config, _) = load_files(&[
        (
            "nextflow.config",
            "params.max_cpus = 2
includeConfig 'base.config'
process.memory = twice(2)
def check_max(obj, type) {
    if (type == 'cpus') {
        try {
            return Math.min(obj, params.max_cpus as int)
        } catch (all) {
            return obj
        }
    }
    return obj
}
def twice(value) {
    value * 2
}
",
        ),
        (
            "base.config",
            "process {\n    cpus = { check_max(4, 'cpus') }\n    time = check_max(8, 'cpus')\n}\n",
        ),
    ]);
    let config = config.unwrap();
    assert_eq!(get(&config, &["process", "memory"]), "4");
    assert_eq!(get(&config, &["process", "time"]), "2");
    assert_eq!(call(&config, &["process", "cpus"]), "2");
}

/// Settings without a value before a setting with one form a chain like `a = b = 5`, and
/// Nextflow 26.04.6 with the legacy parser sets each of them to 5.
#[test]
fn chained_assignments_set_each_setting() {
    let config = load_text("params {\n  a =\n  b =\n  c = 5\n}\nprocess.x = params.y = 'z'\n");
    assert_eq!(get(&config, &["params", "a"]), "5");
    assert_eq!(get(&config, &["params", "b"]), "5");
    assert_eq!(get(&config, &["params", "c"]), "5");
    assert_eq!(get(&config, &["process", "x"]), "z");
    assert_eq!(get(&config, &["params", "y"]), "z");
}

/// `Runtime.runtime.availableProcessors()` gives the processors of this machine, as Java does.
#[test]
fn runtime_processors() {
    let config = load_text(
        "params.a = Runtime.runtime.availableProcessors()\nparams.b = Runtime.getRuntime().availableProcessors()\n",
    );
    let processors = std::thread::available_parallelism().map_or(1, usize::from);
    assert_eq!(get(&config, &["params", "a"]), processors.to_string());
    assert_eq!(get(&config, &["params", "b"]), processors.to_string());
}

/// `includeConfig` inside `profiles` includes the file as at the top, with its profiles, as
/// Nextflow 26.04.6 with the legacy parser sets `top` and `partition` for `-profile local`,
/// and `top` and `s` without a profile.
#[test]
fn includes_inside_profiles() {
    let directory = tempfile::tempdir().unwrap();
    let main =
        "profiles {\n  standard {\n    params.s = 1\n  }\n  includeConfig 'local.config'\n}\n";
    let local = "profiles {\n  local {\n    params.partition = 'local'\n  }\n}\nparams.top = 2\n";
    std::fs::write(directory.path().join("nextflow.config"), main).unwrap();
    std::fs::write(directory.path().join("local.config"), local).unwrap();
    for (profiles, expected) in [
        (Some(vec!["local".to_owned()]), ["partition", "local"]),
        (None, ["s", "1"]),
    ] {
        let options = Options {
            profiles,
            params: Value::map([]),
            project_dir: directory.path().to_string_lossy().into_owned(),
            launch_dir: directory.path().to_string_lossy().into_owned(),
        };
        let mut host = Recorder::default();
        let files = [directory.path().join("nextflow.config")];
        let config = load(&files, &options, &mut host).unwrap();
        assert_eq!(get(&config, &["params", "top"]), "2");
        assert_eq!(get(&config, &["params", expected[0]]), expected[1]);
    }
}

/// `includeConfig` inside a block puts the settings of the file into the block, as Nextflow
/// 26.04.6 with the legacy parser gives `singularity.params.q = 1` for this config.
#[test]
fn includes_inside_blocks() {
    let main = "profiles {\n  standard {\n    singularity {\n      enabled = true\n      includeConfig 'c.config'\n    }\n  }\n}\n";
    let (config, _) = load_files(&[
        ("nextflow.config", main),
        ("c.config", "params.q = 1\nx.y = 2\n"),
    ]);
    let config = config.unwrap();
    assert_eq!(get(&config, &["singularity", "enabled"]), "true");
    assert_eq!(get(&config, &["singularity", "params", "q"]), "1");
    assert_eq!(get(&config, &["singularity", "x", "y"]), "2");
    assert!(config.get(&["params", "q"]).is_none());
}
