//! Checks jobs that run in the one working directory that they share, as in Snakemake.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use workflow_graph::{Attempt, Engine, Executor, Launch, Outcome, OutputFile, Running, Writable};

/// What a job does in the folder where it starts; its exit code.
type Step = fn(&Path) -> i32;

/// Runs each task as its rule's step in the folder where the task starts, and collects the
/// outputs that are there, as the local executor does.
struct Host {
    steps: HashMap<&'static str, Step>,
    store: PathBuf,
    tasks: Vec<workflow_graph::Task>,
}

impl Executor for Host {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = (*attempt.task).clone();
        self.tasks.push(task.clone());
        let mut folder = PathBuf::from(task.workdir.clone().unwrap_or_default());
        // A job of a shadow directory changes into it first.
        if task.command.first().is_some_and(|word| word == "sh")
            && task
                .command
                .get(2)
                .is_some_and(|word| word.contains("cd \"$1\""))
        {
            folder = PathBuf::from(&task.command[4]);
        }
        let exit_code = match self.steps.get(task.step.as_str()) {
            Some(step) => step(&folder),
            None => 0,
        };
        let mut files = Vec::new();
        for output in &task.outputs {
            let path = Path::new(output.trim_end_matches('/'));
            let Ok(metadata) = std::fs::metadata(path) else {
                continue;
            };
            let location = self.store.join(format!("{}-{}", task.id, files.len()));
            if metadata.is_file() {
                std::fs::copy(path, &location).expect("copies an output");
            }
            files.push(OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                directory: metadata.is_dir(),
                ..OutputFile::default()
            });
        }
        Running::ended(Ok(Outcome {
            id: task.id,
            exit_code,
            files,
            ..Outcome::default()
        }))
    }
}

/// A folder with an empty working directory `work`.
fn folder() -> tempfile::TempDir {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::create_dir_all(folder.path().join("work")).expect("a working directory");
    folder
}

#[test]
fn shares_undeclared_files() {
    let snakefile = r#"
rule all:
    input: "b.txt"
rule a:
    output: "a.txt", "link.txt"
    shell: "unused"
rule b:
    input: "a.txt"
    output: "b.txt"
    shell: "unused"
"#;
    let steps: [(&str, Step); 2] = [
        ("a", |folder| {
            std::fs::write(folder.join("a.txt"), "a\n").unwrap();
            std::fs::write(folder.join("undeclared.txt"), "hidden\n").unwrap();
            std::os::unix::fs::symlink("a.txt", folder.join("link.txt")).unwrap();
            0
        }),
        ("b", |folder| {
            let a = std::fs::read_to_string(folder.join("a.txt")).unwrap();
            let hidden = std::fs::read_to_string(folder.join("undeclared.txt")).unwrap();
            std::fs::write(folder.join("b.txt"), a + &hidden).unwrap();
            0
        }),
    ];
    let (folder, host, error) = run(folder(), snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    assert_eq!(
        std::fs::read_to_string(work.join("b.txt")).unwrap(),
        "a\nhidden\n"
    );
    assert!(
        std::fs::symlink_metadata(work.join("link.txt"))
            .unwrap()
            .is_symlink()
    );
    let task = host.tasks.iter().find(|task| task.step == "b").unwrap();
    let place = task
        .inputs
        .iter()
        .find(|input| input.writable == Writable::InPlace);
    assert_eq!(
        place.map(|input| Path::new(&input.source)),
        Some(work.as_path())
    );
    assert_eq!(task.workdir.as_deref().map(Path::new), Some(work.as_path()));
    assert_eq!(task.outputs, [work.join("b.txt").to_string_lossy()]);
}

#[test]
fn touches_outputs_in_place() {
    let snakefile = r#"
rule all:
    input: "flag", "kept.txt"
rule mark:
    output: touch("flag")
    shell: "unused"
rule keep:
    output: protected("kept.txt")
    shell: "unused"
"#;
    let steps: [(&str, Step); 1] = [("keep", |folder| {
        std::fs::write(folder.join("kept.txt"), "kept").unwrap();
        0
    })];
    let (folder, _host, error) = run(folder(), snakefile, &steps);
    assert_eq!(error, None);
    let work = folder.path().join("work");
    assert!(work.join("flag").is_file());
    let kept = std::fs::metadata(work.join("kept.txt")).unwrap();
    assert!(kept.permissions().readonly(), "protected");
    let records = std::fs::read_dir(work.join(".snakemake/iunctus-signatures")).unwrap();
    assert_eq!(records.count(), 2, "both outputs are recorded");
}

/// Runs a Snakefile in the working directory of `folder` with jobs in the shared directory;
/// the folder, the tasks and the error.
fn run(
    folder: tempfile::TempDir,
    snakefile: &str,
    steps: &[(&'static str, Step)],
) -> (tempfile::TempDir, Host, Option<String>) {
    let workdir = folder.path().join("work");
    std::fs::write(workdir.join("Snakefile"), snakefile).expect("a Snakefile");
    let launch = Launch {
        target: workdir,
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let options = snakemake_runtime::RunSettings {
        shared_workdir: true,
        ..Default::default()
    };
    let mut driver = snakemake_runtime::driver_with_options(&launch, Default::default(), options)
        .expect("a driver");
    let store = folder.path().join("store");
    std::fs::create_dir_all(&store).expect("a store");
    let mut host = Host {
        steps: steps.iter().copied().collect(),
        store,
        tasks: Vec::new(),
    };
    let execution = Engine::new(&mut host).run("Snakefile", driver.as_mut());
    let error = execution.error.map(|error| error.to_string());
    (folder, host, error)
}
