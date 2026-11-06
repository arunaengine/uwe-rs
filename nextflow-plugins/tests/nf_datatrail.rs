//! nf-datatrail against the files that nf-datatrail 0.0.1 wrote with Nextflow 26.04.6 for the
//! probe `main.nf`: a detailed graph with its JSON and summary, and plain tag clusters.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

use serde_json::Value;

/// A run of the probe with this `datatrail` scope, where `{out}` is an output folder.
fn run(scope: &str, name: &str) -> (tempfile::TempDir, Option<String>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_datatrail");
    common::copy(&fixtures, folder, &["main.nf"]);
    std::fs::create_dir(folder.join("data")).expect("makes the data folder");
    std::fs::copy(fixtures.join("seed.txt"), folder.join("data/seed.txt")).expect("copies");
    let scope = scope.replace("{out}", &folder.join("out").to_string_lossy());
    let config = format!("plugins {{ id 'nf-datatrail@0.0.1' }}\ndatatrail {{\n{scope}}}\n");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, name);
    (directory, execution.error.map(|error| error.to_string()))
}

/// The two `SAY` tasks under one name, as their order differs between runs.
fn same(text: &str) -> String {
    text.replace("SAY (a)", "SAY (x)")
        .replace("SAY (b)", "SAY (x)")
        .replace("\"cluster_a\"", "\"cluster_x\"")
        .replace("\"cluster_b\"", "\"cluster_x\"")
        .replace("label=\"a\"", "label=\"x\"")
        .replace("label=\"b\"", "label=\"x\"")
}

fn lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = same(text).lines().map(str::to_owned).collect();
    lines.sort();
    lines
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("reads the output")
}

/// The summary without its hash column, which holds uwe's own task hash.
fn without_hash(text: &str, delimiter: char) -> Vec<String> {
    (text.lines())
        .map(|line| {
            let mut fields: Vec<&str> = line.split(delimiter).collect();
            fields.remove(1);
            fields.join(&delimiter.to_string())
        })
        .collect()
}

/// Items of a JSON list as sorted compact text, for comparing lists in another order.
fn sorted(value: &Value) -> Vec<String> {
    let mut items: Vec<String> = (value.as_array().into_iter().flatten())
        .map(|item| {
            same(&item.to_string())
                .replace("\"tag\":\"a\"", "\"tag\":\"x\"")
                .replace("\"tag\":\"b\"", "\"tag\":\"x\"")
        })
        .collect();
    items.sort();
    items
}

#[test]
fn writes_detailed_graph() {
    let scope = "  overwrite = true\n  plot { file = '{out}/dag.dot'; detailed = true }\n  \
                 persist = '{out}/dag.json'\n  input = '{out}/input.csv'\n  output = \
                 '{out}/output.csv'\n  summary = '{out}/summary.csv'\n";
    let (directory, error) = run(scope, "detailed_run");
    assert_eq!(error, None);
    let out = directory.path().join("out");
    let fixtures = common::fixtures("nf_datatrail");
    assert_eq!(
        lines(&read(&out.join("dag.dot"))),
        lines(&read(&fixtures.join("dag.dot")))
    );
    let json = read(&out.join("dag.json"));
    assert!(json.starts_with("{\n    \"origins\": [\n        {\n            \"ID\": \"o0\",\n"));
    let (made, recorded): (Value, Value) = (
        serde_json::from_str(&json).expect("JSON"),
        serde_json::from_str(&read(&fixtures.join("dag.json"))).expect("JSON"),
    );
    for key in ["origins", "processes", "edges"] {
        assert_eq!(sorted(&made[key]), sorted(&recorded[key]), "{key}");
    }
    assert_eq!(
        without_hash(&read(&out.join("summary.csv")), ';'),
        without_hash(&read(&fixtures.join("summary.csv")), ';')
    );
    let inputs = read(&out.join("input.csv"));
    assert_eq!(inputs.lines().next(), Some("name;hash;path;type;size"));
    assert_eq!(inputs.lines().count(), 6, "{inputs}");
    let outputs = read(&out.join("output.csv"));
    assert!(
        outputs
            .lines()
            .any(|line| line.starts_with("COUNT;") && line.ends_with("/count.txt;f;13")),
        "{outputs}"
    );
}

#[test]
fn writes_plain_clusters() {
    let scope = "  delimiter = ','\n  plot { file = '{out}/dag-variant.dot'; detailed = false; \
                 cluster = true; legend = false; rankdir = 'LR'; filter = ['COUNT'] }\n  summary \
                 = '{out}/summary.csv'\n";
    let (directory, error) = run(scope, "plain_run");
    assert_eq!(error, None);
    let out = directory.path().join("out");
    let fixtures = common::fixtures("nf_datatrail");
    let made = read(&out.join("dag-variant.dot"));
    assert_eq!(
        lines(&made),
        lines(&read(&fixtures.join("dag-variant.dot"))),
        "{made}"
    );
    let summary = read(&out.join("summary.csv"));
    assert_eq!(
        summary.lines().next(),
        Some("task,hash,inputs,inputSize,outputs,outputSize,usedBy")
    );
}

#[test]
fn refuses_existing_file() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let existing = directory.path().join("summary.csv");
    std::fs::write(&existing, "old").expect("writes");
    let scope = format!("  summary = '{}'\n", existing.display());
    let (_, error) = run(&scope, "existing_run");
    let error = error.expect("the run stops");
    assert!(
        error.contains(&format!(
            "Summary file already exists: {} -- enable `dag.overwrite` in your config file to \
             overwrite existing summary files",
            existing.display()
        )),
        "{error}"
    );
    let (_, error) = run("  input = '{out}/inputs.txt'\n", "csv_run");
    assert!(error.is_some_and(|error| error.contains("Input file must be a CSV file:")));
}
