//! Prints the wir documents that Snakefiles compile into with `uwe lower`, names the parts
//! that stay native Snakemake ops, and runs compiled Snakefiles through the wir runtime.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;

mod common;

/// Runs `uwe lower Snakefile` on a Snakefile in a folder of its own; its exit code, what it
/// printed on standard output, and on standard error.
fn lower(snakefile: &str) -> (i32, String, String) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    std::fs::write(root.join("Snakefile"), snakefile).expect("writes the Snakefile");
    let output = common::command()
        .args(["lower", "Snakefile"])
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .expect("runs uwe");
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let code = output.status.code().unwrap_or(-1);
    assert!(
        !root.join(".snakemake/locks").exists(),
        "planning takes no lock"
    );
    (code, text(&output.stdout), text(&output.stderr))
}

#[test]
fn prints_compiled_snakefiles() {
    let snakefile = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: temp(\"a.txt\")\n    shell: \"echo a > {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    shell: \"cp {input} {output}\"\n";
    let (code, printed, errors) = lower(snakefile);
    assert_eq!((code, errors.as_str()), (0, ""), "{printed}");
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    assert_eq!(document["sources"][0]["location"], "Snakefile");
    let ops = document["graphs"][0]["ops"].as_array().expect("ops");
    let calls: Vec<&str> = (ops.iter())
        .filter_map(|op| op["kind"]["task"]["builder"]["snakemake_job"]["task"]["call"].as_str())
        .collect();
    assert_eq!(calls, ["a", "b", "all"]);
    assert_eq!(
        document["graphs"][0]["edges"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn names_native_parts() {
    let snakefile = "rule all:\n    input: \"b.txt\"\n\ncheckpoint a:\n    output: \"a.txt\"\n    shell: \"touch {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    shell: \"cp {input} {output}\"\n";
    let (code, printed, errors) = lower(snakefile);
    assert_eq!(code, 0, "{errors}");
    let expected = "uwe: op snakemake stays a native Snakemake op: rule a is a checkpoint\n";
    assert_eq!(errors, expected);
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    let native = &document["graphs"][0]["ops"][0]["kind"]["native"];
    assert_eq!(native["language"], "snakemake");
}

const WORD: &str = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: \"a.txt\"\n    shell: \"echo {config[word]} > {output}\"\n\nrule b:\n    input: \"a.txt\"\n    output: \"b.txt\"\n    shell: \"cp {input} {output}\"\n";

/// Runs `uwe graph <arguments> -lowered -format json` on `snakefile` in a folder of its own,
/// with `files` beside it; the folder and the graph it printed.
fn graph_lowered(
    snakefile: &str,
    files: &[(&str, &str)],
    arguments: &[&str],
) -> (tempfile::TempDir, Json) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    std::fs::write(root.join("Snakefile"), snakefile).expect("writes the Snakefile");
    for (name, text) in files {
        std::fs::write(root.join(name), text).expect("writes a file");
    }
    let output = common::command()
        .args(["graph", "Snakefile", "-lowered", "-format", "json"])
        .args(arguments)
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .expect("runs uwe");
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{errors}");
    let graph = serde_json::from_slice(&output.stdout).expect("prints JSON");
    (folder, graph)
}

/// `uwe graph -lowered` runs the jobs of a compiled Snakefile through the job adapter, with
/// the config values of the command line in the document.
#[test]
fn runs_compiled_snakefiles() {
    let (_folder, graph) = graph_lowered(WORD, &[], &["--config", "word=hello"]);
    let nodes = graph["nodes"].as_array().expect("nodes");
    let names: Vec<&str> = (nodes.iter())
        .filter_map(|node| node["task"]["name"].as_str())
        .collect();
    assert_eq!(names, ["a", "b", "all"]);
    assert!(
        nodes[0]["task"]["script"]
            .as_str()
            .is_some_and(|script| script.contains("echo hello"))
    );
}

/// A config file of `-inputs` is a config source of the document, not an input of its graph.
#[test]
fn takes_config_inputs() {
    let (_folder, graph) = graph_lowered(
        WORD,
        &[("config.yaml", "word: hello\n")],
        &["-inputs", "config.yaml"],
    );
    let nodes = graph["nodes"].as_array().expect("nodes");
    let script = nodes[0]["task"]["script"].as_str().unwrap_or_default();
    assert!(script.contains("echo hello"), "{script}");
}

/// A Snakefile that stays one native op runs in the launch folder, where its inputs are.
#[test]
fn runs_native_parts_in_place() {
    let snakefile = "rule all:\n    input: \"c.txt\"\n\ncheckpoint c:\n    input: \"in.txt\"\n    output: \"c.txt\"\n    shell: \"cp {input} {output}\"\n";
    let (folder, graph) = graph_lowered(snakefile, &[("in.txt", "raw\n")], &[]);
    let nodes = graph["nodes"].as_array().expect("nodes");
    let copy = (nodes.iter())
        .find(|node| node["task"]["step"] == "c")
        .expect("the checkpoint task");
    let source = copy["task"]["inputs"][0]["source"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(std::path::Path::new(source), folder.path().join("in.txt"));
}
