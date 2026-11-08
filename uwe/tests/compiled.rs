//! Prints the wir documents that Snakefiles compile into with `uwe lower`, and names the
//! parts that stay native Snakemake ops.
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
