//! Checks retained run identities and migration of their database records.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_database::{Database, RemoteFile, RunStart, Source, SourceKind, VERSION};
use workflow_graph::{
    Input, Language, Outcome, OutputFile, ReproducibilityCheck, Task, TaskId, Verdict,
};

fn start(id: &str) -> RunStart {
    RunStart {
        id: id.into(),
        resumes: None,
        workflow: "/work/pipeline".into(),
        language: Language::Nextflow,
        revision: Some("0123abc".into()),
        arguments: vec!["run".into(), "pipeline".into()],
        parameters: Vec::new(),
        executor: "local".into(),
        simulated: false,
        store: Some("/cache/store".into()),
        program: Some("uwe 0.1.0".into()),
        launch: Some("/work".into()),
        workflow_folder: Some("/work/pipeline".into()),
        repeats: Some("run-0".into()),
    }
}

fn task(id: u64, image: &str) -> Task {
    Task {
        id: TaskId(id),
        name: format!("STEP ({id})"),
        step: "STEP".into(),
        image: Some(image.into()),
        command: vec!["true".into()],
        inputs: vec![Input {
            path: "reads.fq".into(),
            source: "https://example.org/reads.fq".into(),
            directory: false,
            content: None,
            ..Default::default()
        }],
        outputs: vec!["out.txt".into()],
        ..Task::default()
    }
}

/// Records a real run of two tasks: the first ran, the second came from the cache and ran again
/// for the reproducibility check, which found other bytes.
fn record(path: &Path) {
    let mut recorder = Database::open(path)
        .expect("opens")
        .record(&start("run-1"))
        .expect("records");
    recorder.outputs(&serde_json::json!({"type": "object"}));
    recorder.sources(&[
        Source {
            kind: SourceKind::Workflow,
            path: "main.nf".into(),
            digest: "sha256:aa".into(),
            size: 12,
        },
        Source {
            kind: SourceKind::Named,
            path: "/work/extra.config".into(),
            digest: "sha256:bb".into(),
            size: 3,
        },
    ]);
    for (id, image, ran) in [
        (1, "debian:stable-slim", "debian@sha256:11"),
        (2, "tool:1", "tool:1"),
    ] {
        recorder.requested(&task(id, image), &[]);
        recorder.started(TaskId(id), 1);
        let reproducibility = (id == 2).then(|| ReproducibilityCheck {
            earlier_run: "run-0".into(),
            verdict: Verdict::Differs {
                paths: vec!["out.txt".into()],
            },
        });
        let outcome = Outcome {
            id: TaskId(id),
            image: Some(ran.into()),
            cached: id == 2,
            files: vec![OutputFile {
                path: "out.txt".into(),
                location: format!("/cache/store/runs/run-1/STEP-{id}/1/work/out.txt"),
                digest: Some(format!("sha256:{id}{id}")),
                ..OutputFile::default()
            }],
            inputs: [("reads.fq".to_owned(), "sha256:cc".to_owned())].into(),
            reproducibility,
            ..Outcome::default()
        };
        recorder.finished(&outcome, 1, false);
    }
    recorder.remote_files(&[RemoteFile {
        address: "https://example.org/reads.fq".into(),
        digest: "sha256:cc".into(),
        size: 40,
        fetched: 1_700_000_000_000,
        version: Some("\"etag-1\"".into()),
    }]);
    recorder.ended(None);
    recorder.close().expect("closes");
}

#[test]
fn retains_run_pins() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    record(&path);
    let database = Database::open(&path).expect("opens");
    let run = database.run("run-1").expect("reads").expect("the run");
    assert_eq!(run.program.as_deref(), Some("uwe 0.1.0"));
    assert_eq!(run.launch.as_deref(), Some("/work"));
    assert_eq!(run.workflow_folder.as_deref(), Some("/work/pipeline"));
    assert_eq!(run.repeats.as_deref(), Some("run-0"));
    let sources = database.sources("run-1").expect("reads the sources");
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[1].kind, SourceKind::Named);
    let remote = database
        .remote_files("run-1")
        .expect("reads the remote files");
    assert_eq!(remote[0].version.as_deref(), Some("\"etag-1\""));
    let images = database.images("run-1").expect("reads the images");
    let pairs: Vec<(&str, &str, bool)> = images
        .iter()
        .map(|image| (image.named.as_str(), image.ran.as_str(), image.pinned()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("debian:stable-slim", "debian@sha256:11", true),
            ("tool:1", "tool:1", false)
        ]
    );
    let schema = database.output_schema("run-1").expect("reads");
    assert_eq!(schema, Some(serde_json::json!({"type": "object"})));
    let task = database.task("run-1", 1).expect("reads").expect("the task");
    assert_eq!(task.inputs[0].digest.as_deref(), Some("sha256:cc"));
    assert!(task.checks.is_empty());
    let checked = database.task("run-1", 2).expect("reads").expect("the task");
    assert_eq!(checked.checks.len(), 1);
    assert_eq!(checked.checks[0].verdict, "differs");
    assert_eq!(checked.checks[0].earlier, "run-0");
    assert_eq!(checked.checks[0].differing, ["out.txt"]);
    let references = database
        .references(&["run-1".to_owned()])
        .expect("reads the references");
    for digest in ["sha256:aa", "sha256:bb", "sha256:cc"] {
        assert!(references.digests.contains(digest), "{digest}");
    }
}

#[test]
fn migrates_run_pins() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    drop(Database::open(&path).expect("creates"));
    let connection = rusqlite::Connection::open(&path).expect("opens");
    connection
        .execute_batch(
            "DROP TABLE sources; DROP TABLE remote_files; DROP TABLE checks;
             ALTER TABLE runs DROP COLUMN program; ALTER TABLE runs DROP COLUMN launch;
             ALTER TABLE runs DROP COLUMN workflow_folder; ALTER TABLE runs DROP COLUMN repeats;
             ALTER TABLE runs DROP COLUMN output_schema; ALTER TABLE runs DROP COLUMN replays;
             ALTER TABLE runs DROP COLUMN replays_task; DROP TABLE deliveries;
             ALTER TABLE runs DROP COLUMN process_host; ALTER TABLE runs DROP COLUMN process_boot;
             ALTER TABLE runs DROP COLUMN process_start;
             ALTER TABLE attempts DROP COLUMN accepted;
             PRAGMA user_version = 3;",
        )
        .expect("goes back to version 3");
    drop(connection);
    record(&path);
    let database = Database::open(&path).expect("opens again");
    assert_eq!(database.version().expect("reads the version"), VERSION);
    assert_eq!(database.sources("run-1").expect("reads").len(), 2);
}
