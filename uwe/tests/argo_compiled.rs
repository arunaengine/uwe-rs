//! Runs Argo workflows compiled into wir with `-lowered`, from the template `-entry` chooses.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use run_database::Database;

mod common;

const WORKFLOW: &str = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {name: entries}
spec:
  entrypoint: main
  templates:
  - name: main
    container: {image: alpine:3.20, command: [echo, main]}
  - name: alternate
    container: {image: alpine:3.20, command: [echo, alternate]}
"#;

#[test]
fn runs_chosen_entry() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    std::fs::write(root.join("workflow.yaml"), WORKFLOW).expect("writes the workflow");
    let arguments = [
        "run",
        "-executor",
        "simulator",
        "workflow.yaml",
        "-entry",
        "alternate",
    ];
    let output = common::command()
        .args(arguments)
        .arg("-lowered")
        .current_dir(root)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("runs uwe");
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{errors}");
    let database = Database::open(&cache.join("uwe/runs.sqlite")).expect("opens");
    let run = database.runs(1).expect("reads").remove(0);
    let rows = database.tasks(&run.id).expect("reads the tasks");
    let commands: Vec<Vec<String>> = (rows.iter())
        .map(|row| {
            database
                .task(&run.id, row.id)
                .expect("reads")
                .expect("a task")
        })
        .map(|detail| detail.description.command)
        .collect();
    assert_eq!(commands, [["echo", "alternate"]]);
}
