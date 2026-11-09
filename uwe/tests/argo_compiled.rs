//! Runs Argo workflows compiled into wir, from the template `-entry` chooses.
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

/// A workflow whose step waits for `uwe resume`, which keeps it one native op.
const PAUSED: &str = "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {name: paused}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    steps:\n    - - name: wait\n        template: pause\n  - name: pause\n    suspend: {}\n";

/// A compiled Argo run hears `uwe resume` for a step that waits for it, as an Argo run does.
#[test]
fn hears_resume_requests() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    std::fs::write(root.join("workflow.yaml"), PAUSED).expect("writes the workflow");
    let requests = root.join("cache/scratch/resume/paused-run");
    std::fs::create_dir_all(&requests).expect("makes the folder of requests");
    let request = r#"{"step":"wait","outputs":{}}"#;
    std::fs::write(requests.join("1.json"), request).expect("writes the request");
    let mut settings = uwe::run::Settings::new(root.join("workflow.yaml"));
    settings.executor = Some("local".into());
    settings.database = Some(root.join("runs.sqlite"));
    settings.cache_folder = Some(root.join("cache"));
    settings.output_dir = Some(root.join("outputs"));
    settings.run_id = Some("paused-run".into());
    settings.setup = Some(uwe::setup::Setup::default());
    let mut request = uwe::RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let ran = request
        .run_blocking()
        .unwrap_or_else(|failure| panic!("{}", failure.message));
    assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
}
