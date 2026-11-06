//! nf-schema functions against the real nf-schema 2.5.1 to 3.0.0 with Nextflow 26.04.6, and
//! 3.1.0 against its specification; the fixtures say how they were recorded.
// Copyright (c) 2026 The uwe Contributors
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

/// A host of the fixture directory whose environment sets `NXF_SYNTAX_PARSER=v1`.
struct Legacy(String);

impl Host for Legacy {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }

    fn environment(&self, name: &str) -> Option<String> {
        (name == "NXF_SYNTAX_PARSER").then(|| "v1".to_owned())
    }
}

/// A config that loads this text.
fn loaded(text: &str) -> Config {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    let mut host = Fixtures(String::new());
    nextflow_config::load(&[file], &options, &mut host).expect("loads")
}

/// A config that asks for this version of nf-schema.
fn pinned(version: &str) -> Config {
    loaded(&format!("plugins {{ id 'nf-schema@{version}' }}\n"))
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
    let config = pinned("2.5.1");
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: &directory,
        cli_params: &[],
        workflow: Value::Null,
    };
    let arguments = vec![Value::string(sheet), Value::string(schema)];
    call("nf-schema", "samplesheetToList", arguments, &mut context).map(|rows| rows.to_string())
}

#[test]
fn converts_referenced_rows() {
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
fn rejects_sample_references() {
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
fn rejects_structural_compositions() {
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
fn validates_schema_values() {
    let directory = directory();
    let mut host = Fixtures(directory.clone());
    let config = pinned("2.5.1");
    let mut context = Context {
        host: &mut host,
        config: &config,
        params: Value::map([]),
        project_dir: &directory,
        cli_params: &[],
        workflow: Value::Null,
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
    params_call_in(&pinned("2.5.1"), name, params, arguments)
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
        cli_params: &[],
        workflow: Value::Null,
    };
    call("nf-schema", name, arguments, &mut context).map(|value| value.to_string())
}

#[test]
fn validates_schema_parameters() {
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
fn coerces_lenient_strings() {
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
        work_dir: "/work".into(),
    };
    let mut host = Fixtures(String::new());
    let config = nextflow_config::load(&[file], &options, &mut host).expect("loads");
    assert!(params_call_in(&config, "validateParameters", params(), vec![]).is_ok());
}

/// Nextflow 26.04.6 keeps command-line params as text. nf-schema 2.7.2 and later read them as
/// numbers and booleans for the check; 2.5.1, `cast_cli_params: false` and 2.8.0 with
/// `NXF_SYNTAX_PARSER=v1 NXF_DISABLE_PARAMS_TYPE_DETECTION=1` do not.
#[test]
fn casts_command_line() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/params")
        .to_string_lossy()
        .into_owned();
    let names = ["outdir".to_owned(), "n".to_owned(), "flag".to_owned()];
    let check = |plugin: &str, arguments: Vec<Value>, legacy: bool| {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let file = folder.path().join("nextflow.config");
        std::fs::write(&file, format!("plugins {{ id '{plugin}' }}\n")).expect("writes");
        let options = nextflow_config::Options {
            profiles: None,
            params: Value::map([]),
            project_dir: "/project".into(),
            launch_dir: "/launch".into(),
            work_dir: "/work".into(),
        };
        let mut host = Fixtures(directory.clone());
        let config = nextflow_config::load(&[file], &options, &mut host).expect("loads");
        let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
        let mut parser = Legacy(directory.clone());
        let mut context = Context {
            host: match legacy {
                true => &mut parser,
                false => &mut host,
            },
            config: &config,
            params: Value::map([text("outdir", "res"), text("n", "5"), text("flag", "false")]),
            project_dir: &directory,
            cli_params: &names,
            workflow: Value::Null,
        };
        call("nf-schema", "validateParameters", arguments, &mut context)
            .map_err(|error| error.to_string())
    };
    assert!(check("nf-schema@2.8.0", vec![], false).is_ok());
    assert!(check("nf-schema@2.7.2", vec![], false).is_ok());
    let expected = "* --flag (false): Value is [string] but should be [boolean]\n\
                    * --n (5): Value is [string] but should be [integer]\n\n";
    let error = check("nf-schema@2.5.1", vec![], false).unwrap_err();
    assert!(error.ends_with(expected), "{error}");
    let options = Value::map([(Value::string("cast_cli_params"), Value::Boolean(false))]);
    let error = check("nf-schema@2.8.0", vec![options], false).unwrap_err();
    assert!(error.ends_with(expected), "{error}");
    let error = check("nf-schema@2.8.0", vec![], true).unwrap_err();
    assert!(error.ends_with(expected), "{error}");
}

/// The `workflow` object of the probe, with its container.
fn probe_workflow(container: Value) -> Value {
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let properties = Value::map([
        text("runName", "magical_mahavira"),
        // uwe gives its own engine, which the summary leaves out unless the config
        // enables one.
        text("containerEngine", "docker"),
        (Value::string("container"), container),
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
    Value::Object(Rc::new(RefCell::new(Object {
        class: "nextflow.script.WorkflowMetadata",
        state: State::Workflow(Workflow {
            properties,
            on_complete: Vec::new(),
            on_error: Vec::new(),
        }),
    })))
}

#[test]
fn summarizes_changed_defaults() {
    let workflow = probe_workflow(Value::map([]));
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
        "",
        "Generic options",
        "  n          : 7",
        "",
        "Core Nextflow options",
        "  runName    : magical_mahavira",
        "  launchDir  : /tmp/nfschema2",
        "  workDir    : /tmp/nfschema2/work",
        "  projectDir : /tmp/nfschema2",
        "  userName   : beavis",
        "  profile    : standard",
        "  configFiles: /tmp/nfschema2/nextflow.config",
        "",
        "!! Only displaying parameters that differ from the pipeline defaults !!",
        "------------------------------------------------------",
    ];
    assert_eq!(log, expected.join("\n"));
}

/// The core options show the engine that the config enables and the containers; the log
/// shows the containers as `container.<name>` up to 2.6.1 and leaves them out from 2.7.0 on,
/// where a single container text fails the log, as nf-schema 2.8.0 does.
#[test]
fn summarizes_containers() {
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let map = Value::map([text("FOO", "x"), text("default", "ubuntu")]);
    let values = params(&[("outdir", "res")]);
    let summarize = |version: &str, function: &str, container: Value| {
        let config = loaded(&format!(
            "plugins {{ id 'nf-schema@{version}' }}\ndocker.enabled = true\n"
        ));
        let workflow = probe_workflow(container);
        params_call_in(&config, function, values.clone(), vec![workflow])
    };
    let summary = summarize("2.8.0", "paramsSummaryMap", map.clone()).unwrap();
    assert!(
        summary.contains(
            "Core Nextflow options:[runName:magical_mahavira, containerEngine:docker, \
             container:[FOO:x, default:ubuntu], launchDir:"
        ),
        "{summary}"
    );
    let log = summarize("2.5.1", "paramsSummaryLog", map.clone()).unwrap();
    assert!(
        log.contains(
            "  containerEngine  : docker\n  container.FOO    : x\n  container.default: ubuntu\n"
        ),
        "{log}"
    );
    let log = summarize("2.8.0", "paramsSummaryLog", map).unwrap();
    assert!(
        log.contains("  containerEngine: docker\n  launchDir      : /tmp/nfschema2\n"),
        "{log}"
    );
    let error = summarize("2.8.0", "paramsSummaryLog", Value::string("ubuntu")).unwrap_err();
    assert_eq!(
        error.to_string(),
        "ClassCastException: Cannot cast object 'ubuntu' with class 'java.lang.String' to \
         class 'java.util.Map'"
    );
}

/// From 2.7.0 on, nf-schema checks params that Groovy counts as false, and words patterns and
/// enums without extra brackets from 2.7.3 on.
#[test]
fn checks_falsy_parameters() {
    let falsy = |outdir: &str| {
        Value::map([
            (Value::string("input"), Value::string("")),
            (Value::string("outdir"), Value::string(outdir)),
            (Value::string("n"), Value::integer(0)),
            (Value::string("flag"), Value::Boolean(false)),
            (Value::string("mode"), Value::list(Vec::new())),
        ])
    };
    let config = pinned("2.8.0");
    let expected = ":\n\n* --input (): \"\" does not match regular expression ^\\S+\\.csv$\n\
                    * --mode ([]): Value is [array] but should be [string]\n\
                    * --mode ([]): Expected any of [a, b]\n\n";
    for outdir in ["", "res"] {
        let error = params_call_in(&config, "validateParameters", falsy(outdir), vec![])
            .unwrap_err()
            .to_string();
        assert!(error.ends_with(expected), "{error}");
    }
}

/// Errors come in the order of nf-schema's validator, which reads the schema into Java hash
/// maps; zero shows as nothing, as Nextflow 26.04.6 prints them for these params.
#[test]
fn orders_like_validator() {
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let values = Value::map([
        text("n", "z"),
        text("off", "w"),
        text("d", "y"),
        text("flag", "x"),
        text("e", ""),
        (Value::string("z"), Value::integer(0)),
    ]);
    let options = Value::map([text("parameters_schema", "schema_order.json")]);
    let check = |version: &str| {
        let config = pinned(version);
        let arguments = vec![options.clone()];
        params_call_in(&config, "validateParameters", values.clone(), arguments)
            .unwrap_err()
            .to_string()
    };
    let lines = |shorter: &str| {
        format!(
            ":\n\n* --flag (x): Value is [string] but should be [boolean]\n\
             * --d (y): Value is [string] but should be [integer]\n{shorter}\
             * --z (): 0 is less than 1\n\
             * --n (z): Value is [string] but should be [integer]\n\
             * --off (w): Value is [string] but should be [boolean]\n\n"
        )
    };
    let error = check("2.5.1");
    assert!(error.ends_with(&lines("")), "{error}");
    let error = check("2.8.0");
    let shorter = "* --e (): \"\" is shorter than 3 characters\n";
    assert!(error.ends_with(&lines(shorter)), "{error}");
}

/// The errors of one group follow the Java hash map of the params, not of the schema: by
/// the number of params, which counts the help params from 2.7.0 on, and by the order the
/// params were set, as Nextflow 26.04.6 prints them for these params.
#[test]
fn orders_like_params() {
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let options = Value::map([text("parameters_schema", "schema_order.json")]);
    let check = |version: &str, values: Value| {
        let arguments = vec![options.clone()];
        params_call_in(&pinned(version), "validateParameters", values, arguments)
            .unwrap_err()
            .to_string()
    };
    let mut values = vec![
        text("n", "z"),
        text("off", "w"),
        text("d", "y"),
        text("flag", "x"),
        text("e", ""),
        (Value::string("z"), Value::integer(0)),
    ];
    values.extend(["p1", "p2", "p3", "p4", "p5"].map(|name| text(name, "v")));
    let error = check("2.8.0", Value::map(values));
    let expected = ":\n\n* --flag (x): Value is [string] but should be [boolean]\n\
         * --d (y): Value is [string] but should be [integer]\n\
         * --e (): \"\" is shorter than 3 characters\n\
         * --n (z): Value is [string] but should be [integer]\n\
         * --off (w): Value is [string] but should be [boolean]\n\
         * --z (): 0 is less than 1\n\n";
    assert!(error.ends_with(expected), "{error}");
    let values = Value::map([
        text("off", "w"),
        text("n", "z"),
        text("d", "y"),
        text("flag", "x"),
        text("e", ""),
        (Value::string("z"), Value::integer(0)),
    ]);
    let error = check("2.5.1", values);
    let expected = ":\n\n* --flag (x): Value is [string] but should be [boolean]\n\
         * --d (y): Value is [string] but should be [integer]\n\
         * --z (): 0 is less than 1\n\
         * --off (w): Value is [string] but should be [boolean]\n\
         * --n (z): Value is [string] but should be [integer]\n\n";
    assert!(error.ends_with(expected), "{error}");
}

#[test]
fn validates_known_parameters() {
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
            cli_params: &[],
            workflow: Value::Null,
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
fn checks_ungrouped_parameters() {
    let options = Value::map([(
        Value::string("parameters_schema"),
        Value::string("schema_defs.json"),
    )]);
    assert!(params_call("validateParameters", params(&[]), vec![options]).is_ok());
}

/// The missing names of one `required` keyword come in the order of a Java hash set, as
/// Nextflow 26.04.6 prints them with nf-schema 2.8.0.
#[test]
fn orders_missing_names() {
    let options = Value::map([(
        Value::string("parameters_schema"),
        Value::string("schema_required.json"),
    )]);
    let config = pinned("2.8.0");
    let error = params_call_in(&config, "validateParameters", Value::map([]), vec![options])
        .unwrap_err()
        .to_string();
    assert!(
        error.ends_with("* Missing required parameter(s): zeta, a1, q, beta\n\n"),
        "{error}"
    );
}

/// The folder of the inputs and the outputs that nf-schema 2.7.3, 2.8.0 and 3.0.0 recorded.
fn probe_folder() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nf_schema")
        .to_string_lossy()
        .into_owned()
}

/// A host of the probe folder that keeps what it prints, with an empty environment.
struct Probe {
    launch: String,
    printed: String,
}

impl Host for Probe {
    fn print(&mut self, text: &str) {
        self.printed.push_str(text);
    }

    fn launch_dir(&self) -> String {
        self.launch.clone()
    }

    fn environment(&self, _: &str) -> Option<String> {
        None
    }
}

/// The lines after `===== <name>` of a recorded output, without Nextflow's own log lines, and
/// with exception classes without their package.
fn recorded(file: &str, name: &str) -> String {
    let folder = probe_folder();
    let text = std::fs::read_to_string(Path::new(&folder).join(file)).expect("reads");
    let text = text.replace("<probe>", &folder);
    let header = format!("===== {name}");
    let mut lines = text.lines().skip_while(|line| *line != header).skip(1);
    let mut section = String::new();
    while let Some(line) = lines.next() {
        if line.starts_with("=====") || line.starts_with("[PIPELINE]") {
            break;
        }
        if line.starts_with("[ERROR]") {
            lines.next();
            lines.next();
            continue;
        }
        if line.starts_with("[WARN]") {
            continue;
        }
        let line = match line
            .strip_prefix("!! ")
            .and_then(|rest| rest.split_once(": "))
        {
            Some((class, message)) => {
                let class = class.rsplit('.').next().unwrap_or(class);
                format!("!! {class}: {message}")
            }
            None => line.to_owned(),
        };
        section.push_str(&line);
        section.push('\n');
    }
    section
}

/// A result as the probe scripts print it.
fn printed(result: Result<Value, Error>) -> String {
    match result {
        Ok(value) => format!("{value}\n"),
        Err(error) => {
            let exception = error.exception().expect("an exception");
            format!("!! {}: {}\n", exception.class, exception.message)
        }
    }
}

/// A run of the probe folder with nf-schema of `version` and the params of `common.config`.
struct Session {
    config: Config,
    params: Value,
    cli: Vec<String>,
    workflow: Value,
}

impl Session {
    fn new(version: &str, extra: &str) -> Session {
        Session::with("common.config", version, extra)
    }

    fn with(params: &str, version: &str, extra: &str) -> Session {
        let folder = probe_folder();
        let plugin = match version {
            "" => "nf-schema".to_owned(),
            version => format!("nf-schema@{version}"),
        };
        let config = loaded(&format!(
            "plugins {{ id '{plugin}' }}\nincludeConfig '{folder}/{params}'\n{extra}\n"
        ));
        let params = config.params();
        Session {
            config,
            params,
            cli: Vec::new(),
            workflow: Value::Null,
        }
    }

    fn call(&self, name: &str, arguments: Vec<Value>) -> (String, String) {
        let folder = probe_folder();
        let mut host = Probe {
            launch: folder.clone(),
            printed: String::new(),
        };
        let mut context = Context {
            host: &mut host,
            config: &self.config,
            params: self.params.clone(),
            project_dir: &folder,
            cli_params: &self.cli,
            workflow: self.workflow.clone(),
        };
        let result = match name {
            "start" => nextflow_plugins::start(&mut context).map(|()| Value::Null),
            name => call("nf-schema", name, arguments, &mut context),
        };
        (printed(result), host.printed)
    }
}

fn options(entries: &[(&str, Value)]) -> Value {
    Value::map((entries.iter()).map(|(key, value)| (Value::string(*key), value.clone())))
}

/// `paramsHelp` in its short, full, parameter and text forms, as 2.7.3, 2.8.0 and 3.0.0 print it.
#[test]
fn help_matches_recordings() {
    for (version, file) in [
        ("2.7.3", "out-2.7.3.txt"),
        ("2.8.0", "out-2.8.0.txt"),
        ("3.0.0", "out-3.0.0.txt"),
    ] {
        let session = Session::new(version, "");
        let parameter = |name: &str| match version {
            "3.0.0" => vec![options(&[("parameter", Value::string(name))])],
            _ => vec![options(&[]), Value::string(name)],
        };
        let texts = options(&[
            ("beforeText", Value::string("BEFORE")),
            ("afterText", Value::string("AFTER")),
            ("command", Value::string("nextflow run probe --input x.csv")),
            ("showHidden", Value::Boolean(true)),
        ]);
        for (name, arguments) in [
            ("help short", vec![]),
            (
                "help full",
                vec![options(&[("fullHelp", Value::Boolean(true))])],
            ),
            ("help parameter tool", parameter("tool")),
            ("help parameter input", parameter("input")),
            ("help missing", parameter("nope")),
            ("help texts", vec![texts]),
        ] {
            let (output, _) = session.call("paramsHelp", arguments);
            assert_eq!(output, recorded(file, name), "{version} {name}");
        }
    }
}

/// A summary map as Groovy's `JsonOutput.prettyPrint` writes it.
fn pretty(value: &Value, depth: usize) -> String {
    match value {
        Value::Map(map) => {
            let pad = "    ".repeat(depth + 1);
            let items: Vec<String> = (map.borrow().entries.iter())
                .map(|(key, value)| format!("{pad}\"{}\": {}", key.0, pretty(value, depth + 1)))
                .collect();
            format!("{{\n{}\n{}}}", items.join(",\n"), "    ".repeat(depth))
        }
        Value::Null => "null".to_owned(),
        other => format!("\"{other}\""),
    }
}

/// The `workflow` object of the summary probes, whose folders the mask hides.
fn summary_workflow(run: &str, config: &str) -> Value {
    let folder = "/home/someone/projects/uwe-rs/.claude/plans/plugin-specs/scratch/nf-schema";
    let text = |name: &str, value: &str| (Value::string(name), Value::string(value));
    let properties = Value::map([
        text("runName", run),
        text("container", "quay.io/biocontainers/fastqc:0.12.1"),
        text("launchDir", folder),
        text("workDir", &format!("{folder}/work")),
        text("projectDir", folder),
        text("userName", "<user>"),
        text("profile", "standard"),
        (
            Value::string("configFiles"),
            Value::list(vec![Value::string(format!("{folder}/{config}"))]),
        ),
    ]);
    Value::Object(Rc::new(RefCell::new(Object {
        class: "nextflow.script.WorkflowMetadata",
        state: State::Workflow(Workflow {
            properties,
            on_complete: Vec::new(),
            on_error: Vec::new(),
        }),
    })))
}

/// Summaries with masks, `hideParams`, a text container and nested params: 3.0.0 leaves a
/// null param out, 2.7.3 and 2.8.0 fail on it, and fail their log on a text container.
#[test]
fn summaries_match_recordings() {
    for (version, params, file, run, config) in [
        (
            "3.0.0",
            "summary.config",
            "sum-3.0.0.txt",
            "big_einstein",
            "s3.0.0.config",
        ),
        (
            "2.7.3",
            "summary.config",
            "sum-2.7.3.txt",
            "x",
            "s2.7.3.config",
        ),
        (
            "3.0.0",
            "summary2.config",
            "sumt-3.0.0.txt",
            "compassionate_jang",
            "t3.0.0.config",
        ),
        (
            "2.8.0",
            "summary2.config",
            "sumt-2.8.0.txt",
            "compassionate_fermat",
            "t2.8.0.config",
        ),
    ] {
        let mut session = Session::with(params, version, "");
        session.workflow = summary_workflow(run, config);
        let modern = version == "3.0.0";
        let with_workflow = |mut arguments: Vec<Value>| {
            if !modern {
                arguments.push(session.workflow.clone());
            }
            arguments
        };
        let folder = probe_folder();
        let mut host = Probe {
            launch: folder.clone(),
            printed: String::new(),
        };
        let mut context = Context {
            host: &mut host,
            config: &session.config,
            params: session.params.clone(),
            project_dir: &folder,
            cli_params: &[],
            workflow: session.workflow.clone(),
        };
        let map = call(
            "nf-schema",
            "paramsSummaryMap",
            with_workflow(vec![]),
            &mut context,
        );
        let map = match map {
            Ok(map) => format!("{}\n", pretty(&map, 0)),
            Err(error) => printed(Err(error)),
        };
        assert_eq!(map, recorded(file, "summary map"), "{file}");
        let log_options = options(&[
            ("beforeText", Value::string("OPT-BEFORE\n")),
            ("afterText", Value::string("\nOPT-AFTER")),
        ]);
        for (name, arguments) in [
            ("summary log", vec![]),
            ("summary log options", vec![log_options]),
        ] {
            let (output, _) = session.call("paramsSummaryLog", with_workflow(arguments));
            let expected =
                recorded(file, name).replace("GroovyCastException", "ClassCastException");
            assert_eq!(output, expected, "{file} {name}");
        }
    }
}

/// The null params of `common.config` with defaults: 2.7.3 and 2.8.0 fail on any, 3.0.0 only
/// on one whose default has `$projectDir`.
#[test]
fn summarizes_null_defaults() {
    for (version, file) in [
        ("2.7.3", "out-2.7.3.txt"),
        ("2.8.0", "out-2.8.0.txt"),
        ("3.0.0", "out-3.0.0.txt"),
    ] {
        let mut session = Session::new(version, "");
        session.workflow = summary_workflow("x", "v.config");
        let mut arguments = vec![];
        if version != "3.0.0" {
            arguments.push(session.workflow.clone());
        }
        for (name, function) in [
            ("summary map", "paramsSummaryMap"),
            ("summary log", "paramsSummaryLog"),
        ] {
            let (output, _) = session.call(function, arguments.clone());
            assert_eq!(output, recorded(file, name), "{version} {name}");
        }
    }
}

/// `validateParameters` warns for the unknown `--ref`, and `samplesheetToList` converts the
/// sheet with its texts as they are.
#[test]
fn validates_like_recordings() {
    for (version, file) in [
        ("2.7.3", "out-2.7.3.txt"),
        ("2.8.0", "out-2.8.0.txt"),
        ("3.0.0", "out-3.0.0.txt"),
    ] {
        let session = Session::new(version, "");
        let (output, warned) = session.call("validateParameters", vec![]);
        assert_eq!(output.replace("null", "valid"), recorded(file, "validate"));
        assert_eq!(
            warned,
            "WARN: The following invalid input values have been detected:\n\n* --ref: genome.fa\n\n"
        );
        let arguments = vec![
            Value::string("samplesheet.csv"),
            Value::string("assets/schema_input.json"),
        ];
        let (output, _) = session.call("samplesheetToList", arguments);
        assert_eq!(output, recorded(file, "samplesheet"), "{version}");
    }
}
