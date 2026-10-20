//! Native validation gates runner publication and database resume eligibility.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use content_store::Store;
use executor::manifest::Collected;
use executor::{Backend, ExecutorError, Finished, Job};
use run_database::{Database, RunStart};
use task_runner::{Runner, RunnerExecutor, Settings};
use workflow_graph::{CancellationToken, Driver, Engine, Execution, Language};

const IMAGE: &str = "image@sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Copy)]
enum Flavor {
    Snakemake,
    Cwl,
}

struct Fake {
    scratch: PathBuf,
    flavor: Flavor,
    calls: AtomicUsize,
}

impl Backend for Fake {
    async fn pin(&self, _image: &str) -> Result<Option<String>, ExecutorError> {
        Ok(Some(IMAGE.into()))
    }

    async fn run(
        &self,
        job: Job<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let valid = call > 0;
        let root = self.scratch.join(job.name);
        let work = root.join("work");
        let control = root.join("control");
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&control)?;
        std::fs::write(control.join("stdout"), "preserved stdout\n")?;
        std::fs::write(control.join("stderr"), "preserved stderr\n")?;
        let (path, bytes) = match self.flavor {
            Flavor::Snakemake => ("out.txt", if valid { "valid\n" } else { "" }),
            Flavor::Cwl => (
                "cwl.output.json",
                if valid {
                    r#"{"n":3}"#
                } else {
                    r#"{"n":"wrong"}"#
                },
            ),
        };
        std::fs::write(work.join(path), bytes)?;
        Ok(Finished {
            address: None,
            exit_code: Some(0),
            files: vec![Collected {
                path: path.into(),
                directory: false,
            }],
            root,
            usage: None,
        })
    }
}

fn executor(root: &Path, store: Store, flavor: Flavor) -> RunnerExecutor<Fake> {
    RunnerExecutor::new(
        Runner::new(
            store,
            Fake {
                scratch: root.join("scratch"),
                flavor,
                calls: AtomicUsize::new(0),
            },
            "acceptance",
            Settings {
                downloads: root.join("downloads"),
                import_inputs: false,
                storage: storage::Storage::default(),
                digests: None,
            },
        ),
        Some("image:latest".into()),
    )
}

fn start(id: &str, language: Language, root: &Path) -> RunStart {
    RunStart {
        id: id.into(),
        resumes: None,
        workflow: root.join("workflow").to_string_lossy().into_owned(),
        language,
        revision: None,
        arguments: vec!["run".into()],
        parameters: Vec::new(),
        executor: "fake".into(),
        simulated: false,
        store: Some(root.join("store").to_string_lossy().into_owned()),
        program: None,
        launch: Some(root.to_string_lossy().into_owned()),
        workflow_folder: Some(root.to_string_lossy().into_owned()),
        repeats: None,
    }
}

fn run(
    database: &Path,
    id: &str,
    language: Language,
    driver: &mut dyn Driver,
    executor: &mut RunnerExecutor<Fake>,
    root: &Path,
) -> Execution {
    let mut recorder = Database::open(database)
        .expect("opens the database")
        .record(&start(id, language, root))
        .expect("starts recording");
    let execution = Engine::new(executor)
        .observer(&mut recorder)
        .run("workflow", driver);
    recorder.close().expect("closes the recording");
    execution
}

fn cache_entries(store: &Store) -> usize {
    workflow_graph::runtime()
        .expect("a runtime")
        .block_on(store.objects("ac/v1/"))
        .expect("lists cache entries")
        .into_iter()
        .filter(|object| !object.folder && object.location.ends_with(".json"))
        .count()
}

fn assert_recorded(database: &Path, run: &str, accepted: bool, resumable: bool) {
    let database = Database::open(database).expect("opens the database");
    let task = database
        .task(run, 1)
        .expect("reads the task")
        .expect("a task");
    assert_eq!(task.attempts[0].exit_code, Some(0));
    assert_eq!(task.attempts[0].accepted, Some(accepted));
    for (stream, expected) in [
        ("stdout", "preserved stdout\n"),
        ("stderr", "preserved stderr\n"),
    ] {
        assert_eq!(
            database
                .log(run, 1, 1, stream)
                .expect("reads a log")
                .expect("a log")
                .text,
            expected
        );
    }
    let reusable = database
        .reusable(&[run.to_owned()])
        .expect("checks resume eligibility");
    assert_eq!(reusable.contains_key(&task.fingerprint), resumable);
}

fn exercise(
    flavor: Flavor,
    language: Language,
    root: &Path,
    mut driver: impl FnMut() -> Box<dyn Driver>,
) {
    let store = Store::local(&root.join("store")).expect("a store");
    let database = root.join("runs.sqlite");
    let mut executor = executor(root, store.clone(), flavor);

    let rejected = run(
        &database,
        "rejected",
        language,
        driver().as_mut(),
        &mut executor,
        root,
    );
    assert!(rejected.error.is_some(), "native rejection fails the run");
    assert_eq!(rejected.graph.nodes[0].exit_code, Some(0));
    assert_eq!(rejected.graph.nodes[0].accepted, Some(false));
    assert_eq!(cache_entries(&store), 0);
    assert_recorded(&database, "rejected", false, false);

    let accepted = run(
        &database,
        "accepted",
        language,
        driver().as_mut(),
        &mut executor,
        root,
    );
    assert!(accepted.error.is_none(), "{:?}", accepted.error);
    assert_eq!(accepted.graph.nodes[0].accepted, Some(true));
    assert!(!accepted.graph.nodes[0].reused);
    assert_eq!(cache_entries(&store), 1);
    assert_recorded(&database, "accepted", true, true);

    if matches!(flavor, Flavor::Snakemake) {
        std::fs::remove_file(root.join("out.txt")).expect("removes the delivered output");
    }
    let reused = run(
        &database,
        "reused",
        language,
        driver().as_mut(),
        &mut executor,
        root,
    );
    assert!(reused.error.is_none(), "{:?}", reused.error);
    assert_eq!(reused.graph.nodes[0].accepted, Some(true));
    assert!(reused.graph.nodes[0].reused);
    assert_eq!(executor.runner().backend().calls.load(Ordering::SeqCst), 2);
    assert_eq!(cache_entries(&store), 1);
    assert_recorded(&database, "reused", true, true);
}

#[test]
fn snakemake_acceptance_gates() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        folder.path().join("Snakefile"),
        r#"
rule all:
    input: "out.txt"
rule make:
    output: ensure("out.txt", non_empty=True)
    shell: "unused"
"#,
    )
    .expect("writes the Snakefile");
    let launch = workflow_graph::Launch {
        target: folder.path().to_owned(),
        scratch: folder.path().join("driver-scratch"),
        ..workflow_graph::Launch::default()
    };
    exercise(
        Flavor::Snakemake,
        Language::Snakemake,
        folder.path(),
        || snakemake_runtime::driver(&launch).expect("a Snakemake driver"),
    );
}

#[test]
fn cwl_acceptance_gates() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: unused\n\
                inputs: []\noutputs: {n: int}\n";
    let target = folder.path().join("main.cwl");
    std::fs::write(&target, tool).expect("writes the CWL tool");
    let launch = workflow_graph::Launch {
        target,
        scratch: folder.path().join("driver-scratch"),
        output_dir: Some(folder.path().join("results")),
        ..workflow_graph::Launch::default()
    };
    exercise(Flavor::Cwl, Language::Cwl, folder.path(), || {
        cwl_runtime::driver(&launch).expect("a CWL driver")
    });
}
