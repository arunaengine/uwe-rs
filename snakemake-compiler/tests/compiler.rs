//! Compiles small Snakefiles and checks the ops, edges, effects and kept parts of the wir
//! documents, which the verifier of wir accepts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use snakemake_runtime::{RunSettings, Settings};
use workflow_graph::Launch;

const SNAKEFILE: &str = r#"
SAMPLES = ["a", "b/c"]

rule all:
    input: expand("counts/{s}.txt", s=SAMPLES)

rule make:
    output: temp("raw/{s}.txt")
    shell: "echo {wildcards.s} > {output}"

rule count:
    input: "raw/{s}.txt"
    output: protected("counts/{s}.txt")
    log: "logs/{s}.log"
    shell: "wc -c {input} > {output} 2> {log}"
"#;

fn compiled(snakefile: &str) -> (tempfile::TempDir, snakemake_compiler::Compiled, Json) {
    compiled_with(snakefile, Launch::default())
}

/// Compiles `snakefile` with the settings of `launch`.
fn compiled_with(
    snakefile: &str,
    launch: Launch,
) -> (tempfile::TempDir, snakemake_compiler::Compiled, Json) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(folder.path().join("Snakefile"), snakefile).expect("writes the Snakefile");
    let launch = Launch {
        target: folder.path().join("Snakefile"),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..launch
    };
    let run = RunSettings {
        workdir: Some(folder.path().to_owned()),
        ..RunSettings::default()
    };
    let base = folder.path().to_owned();
    let compiled =
        snakemake_compiler::compile(&launch, Settings::default(), run, &base).expect("compiles");
    if let Err(problems) = wir::verify(&compiled.document) {
        panic!("{problems:?}");
    }
    let json = serde_json::to_value(&compiled.document).expect("serializes");
    (folder, compiled, json)
}

fn op<'a>(document: &'a Json, path: &str) -> &'a Json {
    (document["graphs"][0]["ops"].as_array().expect("ops").iter())
        .find(|op| op["path"] == path)
        .unwrap_or_else(|| panic!("no op {path}"))
}

#[test]
fn compiles_jobs() {
    let (_folder, compiled, document) = compiled(SNAKEFILE);
    assert!(compiled.full(), "{:?}", compiled.kept);
    let source = &document["sources"][0];
    assert_eq!(
        (&source["id"], &source["location"]),
        (&json!("snakefile"), &json!("Snakefile"))
    );
    assert!(
        source["digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
    let paths: Vec<&str> = (document["graphs"][0]["ops"].as_array().expect("ops").iter())
        .filter_map(|op| op["path"].as_str())
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        ["all", "count/a", "count/b_c", "make/a", "make/b_c"]
    );
    let count = op(&document, "count/b_c");
    let builder = &count["kind"]["task"]["builder"]["snakemake_job"];
    assert_eq!(builder["wildcards"], json!({"s": "b/c"}));
    assert_eq!(builder["task"]["call"], "count");
    assert_eq!(builder["task"]["entry"], "all");
    assert_eq!(
        builder["task"]["inputs"],
        json!([{"port": "input_0", "native": "input/0"}])
    );
    assert_eq!(
        count["origin"],
        json!({"source": "snakefile", "at": "count"})
    );
    let published = json!([{"publish": "counts/b/c.txt"}, {"publish": "logs/b/c.log"}]);
    assert_eq!(count["effects"], published);
    assert!(
        op(&document, "make/a").get("effects").is_none(),
        "temporary outputs stay"
    );
    let edge = json!({
        "from": {"op": "make/b_c", "port": "output_0"},
        "to": {"op": "count/b_c", "port": "input_0"},
    });
    assert!(
        document["graphs"][0]["edges"]
            .as_array()
            .expect("edges")
            .contains(&edge)
    );
    let all = op(&document, "all");
    assert_eq!(all["inputs"].as_array().map(Vec::len), Some(2));
}

#[test]
fn keeps_checkpoints() {
    let snakefile = "rule all:\n    input: \"b.txt\"\n\ncheckpoint a:\n    output: \"a.txt\"\n    shell: \"touch {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    shell: \"cp {input} {output}\"\n";
    let (_folder, compiled, document) = compiled(snakefile);
    assert_eq!(compiled.kept.len(), 1);
    assert_eq!(compiled.kept[0].reason, "rule a is a checkpoint");
    let native = &op(&document, "snakemake")["kind"]["native"];
    assert_eq!(native["source"], "snakefile");
    assert!(native.get("entry").is_none());
}

#[test]
fn compiles_groups() {
    let snakefile = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: \"a.txt\"\n    group: \"g\"\n    shell: \"touch {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    group: \"g\"\n    shell: \"cp {input} {output}\"\n";
    let (_folder, compiled, document) = compiled(snakefile);
    assert!(compiled.full(), "{:?}", compiled.kept);
    let group = op(&document, "a");
    let builder = &group["kind"]["task"]["builder"]["snakemake_job"];
    assert_eq!(
        builder["group"],
        json!([{"source": "snakefile", "rule": "b"}])
    );
    assert!(group.get("inputs").is_none(), "{group}");
    assert_eq!(group["outputs"].as_array().map(Vec::len), Some(2));
    let edge =
        json!({"from": {"op": "a", "port": "output_1"}, "to": {"op": "all", "port": "input_0"}});
    assert_eq!(document["graphs"][0]["edges"], json!([edge]));
}

#[test]
fn compiles_planning_commands() {
    let snakefile = "shell(\"echo planned\")\n\nrule all:\n    output: \"a.txt\"\n    shell: \"touch {output}\"\n";
    let (_folder, compiled, _) = compiled(snakefile);
    assert!(compiled.full(), "{:?}", compiled.kept);
}

const TWO: &str = "rule a:\n    output: \"a.txt\"\n    params: x=config[\"x\"]\n    shell: \"touch {output}\"\n\nrule b:\n    output: \"b.txt\"\n    shell: \"touch {output}\"\n";

#[test]
fn compiles_plan_settings() {
    let launch = Launch {
        targets: vec!["a.txt".into(), "b.txt".into()],
        params: vec![("x".into(), "{y: 1}".into())],
        forcerun: vec!["a".into()],
        ..Launch::default()
    };
    let (folder, compiled, document) = compiled_with(TWO, launch);
    assert!(compiled.full(), "{:?}", compiled.kept);
    let builder = &op(&document, "a")["kind"]["task"]["builder"]["snakemake_job"];
    assert_eq!(builder["task"]["entry"], "a.txt");
    assert_eq!(builder["targets"], json!(["a.txt", "b.txt"]));
    assert_eq!(builder["forcerun"], json!(["a"]));
    let values = (document["sources"].as_array().expect("sources").iter())
        .find(|source| source["id"] == builder["config_values"])
        .expect("a source of config values");
    assert!(values["digest"].is_string(), "{values}");
    let wir::Kind::Task(task) = &compiled.document.graphs[0].ops[0].kind else {
        panic!("no task");
    };
    let Some(wir::TaskBuilder::SnakemakeJob(rule)) = &task.builder else {
        panic!("no job");
    };
    let launch =
        snakemake_compiler::job_launch(&compiled.document, rule, folder.path()).expect("a launch");
    assert_eq!(launch.params, [("x".to_owned(), "{y: 1}".to_owned())]);
    assert_eq!((launch.targets.len(), launch.forcerun.len()), (2, 1));
    assert_eq!(launch.target, folder.path().join("Snakefile"));
}

#[test]
fn keeps_dropped_settings() {
    let snakefile =
        format!("checkpoint c:\n    output: \"c.txt\"\n    shell: \"touch {{output}}\"\n\n{TWO}");
    let launch = Launch {
        targets: vec!["c.txt".into()],
        params: vec![("x".into(), "1".into())],
        ..Launch::default()
    };
    let (_folder, compiled, document) = compiled_with(&snakefile, launch);
    let reasons: Vec<&str> = compiled
        .kept
        .iter()
        .map(|kept| kept.reason.as_str())
        .collect();
    assert_eq!(
        reasons,
        [
            "rule c is a checkpoint",
            "it drops config values of the command line"
        ]
    );
    assert_eq!(document["sources"].as_array().map(Vec::len), Some(1));
}
