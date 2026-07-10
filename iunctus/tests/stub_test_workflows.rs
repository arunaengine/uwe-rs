//! Runs nf-core pipelines of the test workflows with `-stub` and the simulator, and compares
//! each task's script in the execution graph with the one Nextflow wrote, as
//! `tests/fixtures/oracle/stub/record.sh` recorded them. Run with `just stub-test-workflows-test`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use workflow_graph::ExecutionGraph;

mod common;

/// The name of a task as a file name, as `record.sh` makes it.
fn file_name(task: &str) -> String {
    let mut name: String = task
        .chars()
        .map(|c| {
            if matches!(c, ':' | ' ' | '(' | ')') {
                '_'
            } else {
                c
            }
        })
        .collect();
    while name.contains("__") {
        name = name.replace("__", "_");
    }
    format!("{}.sh", name.trim_end_matches('_'))
}

/// The scripts of a directory of recorded tasks, by file name.
fn read_scripts(files: impl Iterator<Item = (String, PathBuf)>) -> BTreeMap<String, String> {
    files
        .map(|(name, path)| (name, std::fs::read_to_string(path).expect("reads a script")))
        .collect()
}

/// The script of each task of a graph, by file name.
fn graph_scripts(file: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(file).expect("reads the graph");
    let graph: ExecutionGraph = serde_json::from_str(&text).expect("the graph as JSON");
    graph
        .nodes
        .into_iter()
        .map(|node| {
            (
                file_name(&node.task.name),
                node.task.script.unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
#[ignore = "needs the nf-core test workflows; run `just stub-test-workflows-test`"]
fn stub_scripts_equal_the_oracle() {
    let test_workflows = PathBuf::from(
        std::env::var("NF_CORE_TEST_WORKFLOWS").expect("NF_CORE_TEST_WORKFLOWS is set"),
    )
    .join("pipelines");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle/stub");
    let mut pipelines: Vec<PathBuf> = std::fs::read_dir(&fixtures)
        .expect("reads the fixtures")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.is_dir())
        .collect();
    pipelines.sort();
    let mut problems = Vec::new();
    for pipeline in pipelines {
        let name = pipeline
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        let cache = tempfile::tempdir().expect("a temporary directory");
        let graph = cache.path().join("graph.json");
        let output = common::command()
            .args(["run", &test_workflows.join(&name).to_string_lossy()])
            .args(["-profile", "test,docker", "-stub"])
            .args(["-graph", &graph.to_string_lossy()])
            .args(["--outdir", "/tmp/iunctus-stub-out"])
            .env("XDG_CACHE_HOME", cache.path())
            .current_dir(cache.path())
            .output()
            .expect("runs iunctus");
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            problems.push(format!("{name}: the run failed: {}", error.trim()));
            continue;
        }
        let expected = read_scripts(std::fs::read_dir(&pipeline).expect("reads").map(|entry| {
            let path = entry.expect("an entry").path();
            (
                path.file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned(),
                path,
            )
        }));
        let actual = graph_scripts(&graph);
        for (task, script) in &expected {
            match actual.get(task) {
                None => problems.push(format!("{name}: no task {task}")),
                Some(mine) if mine != script => problems.push(format!("{name}: {task} differs")),
                Some(_) => {}
            }
        }
        for task in actual.keys().filter(|task| !expected.contains_key(*task)) {
            problems.push(format!("{name}: an extra task {task}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
