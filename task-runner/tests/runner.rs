//! Checks the cache, the staging and the stored outputs of the runner with a fake executor.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::Path;

use content_store::Store;
use executor::manifest::Collected;
use executor::{Executor, ExecutorError, Finished, Staging};
use task_model::{FileKind, TaskRequest, TaskSpec};
use task_runner::Runner;
use workflow_graph::{Input, Task, TaskId};

/// Writes each output pattern as a file with the first staged file's content, or for a pattern
/// that ends with `/` as a directory with that file and an empty folder. It prints `said` and
/// remembers each run and what it staged.
struct Fake {
    scratch: std::path::PathBuf,
    runs: RefCell<Vec<String>>,
    staged: RefCell<Vec<Staging>>,
    exit_code: i32,
}

impl Executor for Fake {
    fn run(
        &self,
        name: &str,
        spec: &TaskSpec,
        staging: &Staging,
    ) -> Result<Finished, ExecutorError> {
        self.runs.borrow_mut().push(name.to_owned());
        self.staged.borrow_mut().push(staging.clone());
        let mut finished = Finished {
            exit_code: Some(self.exit_code),
            files: Vec::new(),
            root: self.scratch.join(name),
        };
        std::fs::create_dir_all(finished.root.join("work"))?;
        std::fs::create_dir_all(finished.root.join("control"))?;
        std::fs::write(finished.stdout(), "said\n")?;
        let input = match staging.inputs.first() {
            Some(file) => std::fs::read_to_string(&file.source)?,
            None => String::new(),
        };
        for rule in &spec.outputs {
            let path = rule.pattern.trim_end_matches('/').to_owned();
            let local = finished.local_path(&path);
            let directory = rule.pattern.ends_with('/');
            if directory {
                std::fs::create_dir_all(local.join("empty"))?;
                std::fs::write(local.join("a.txt"), &input)?;
            } else {
                std::fs::create_dir_all(local.parent().expect("a parent"))?;
                std::fs::write(&local, &input)?;
            }
            finished.files.push(Collected {
                path,
                directory,
                size: if directory { 0 } else { input.len() as u64 },
            });
        }
        Ok(finished)
    }
}

fn runner(root: &Path, exit_code: i32) -> Runner<Fake> {
    let store = Store::local(&root.join("store")).expect("a store");
    let fake = Fake {
        scratch: root.join("scratch"),
        runs: RefCell::new(Vec::new()),
        staged: RefCell::new(Vec::new()),
        exit_code,
    };
    Runner::new(store, fake, "run1", &root.join("downloads")).expect("a runner")
}

/// Task 1 of step `P`, which reads `input.txt` and writes `out.txt`.
fn task(root: &Path, command: &str) -> Task {
    let input = root.join("input.txt");
    std::fs::write(&input, "data\n").expect("writes the input");
    Task {
        id: TaskId(1),
        name: "P (1)".into(),
        step: "P".into(),
        image: Some("debian".into()),
        command: vec!["/bin/bash".into(), "-c".into(), command.into()],
        inputs: vec![Input {
            path: "input.txt".into(),
            source: input.to_string_lossy().into_owned(),
            directory: false,
            content: None,
        }],
        outputs: vec!["out.txt".into()],
        ..Task::default()
    }
}

fn request(runner: &Runner<Fake>, task: &Task) -> TaskRequest {
    runner
        .request(task, Some("debian@sha256:00".into()))
        .expect("a request")
}

#[test]
fn reuses_shared_results() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let first = runner
        .execute(&request(&runner, &task(root.path(), "a")))
        .expect("runs");
    assert!(!first.reused);
    let [output] = first.files.as_slice() else {
        panic!("one output")
    };
    assert_eq!(output.location, "runs/run1/P-1/1/work/out.txt");
    let stored = runner.local_path(&output.location).expect("a local store");
    assert_eq!(std::fs::read_to_string(stored).expect("reads"), "data\n");
    let said = runner
        .read_content(first.stdout.as_ref().expect("a standard output"))
        .expect("reads");
    assert_eq!(said, "said\n");
    let second = runner
        .execute(&request(&runner, &task(root.path(), "a")))
        .expect("runs");
    assert!(second.reused);
    assert_eq!(second.files, first.files);
    assert_eq!(second.stdout, first.stdout);
    let third = runner
        .execute(&request(&runner, &task(root.path(), "b")))
        .expect("runs");
    assert!(!third.reused);
    assert_eq!(runner.executor().runs.borrow().len(), 2);
    // Each run removes its scratch directory.
    assert!(!root.path().join("scratch/run1-P-1-1").exists());
}

#[test]
fn failures_are_not_cached() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 3);
    let first = runner
        .execute(&request(&runner, &task(root.path(), "a")))
        .expect("runs");
    assert_eq!(first.exit_code, Some(3));
    let second = runner
        .execute(&request(&runner, &task(root.path(), "a")))
        .expect("runs");
    assert!(!second.reused);
}

#[test]
fn directories_travel_as_one_entry() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut first = task(root.path(), "a");
    first.outputs = vec!["db/".into(), "/tmp/result.txt".into()];
    let result = runner.execute(&request(&runner, &first)).expect("runs");
    let [db, absolute] = result.files.as_slice() else {
        panic!("two outputs")
    };
    assert!(db.directory && !absolute.directory);
    assert_eq!(db.location, "runs/run1/P-1/1/work/db");
    assert_eq!(absolute.location, "runs/run1/P-1/1/root/tmp/result.txt");
    assert_eq!(db.size, 5);
    let on_disk = runner.local_path(&db.location).expect("a local store");
    assert_eq!(
        std::fs::read_to_string(on_disk.join("a.txt")).expect("reads"),
        "data\n"
    );
    assert!(on_disk.join("empty").is_dir());

    // The directory goes back in as an input, with its empty folder and standard input.
    let mut second = task(root.path(), "b");
    second.id = TaskId(2);
    second.inputs.push(Input {
        path: "in".into(),
        source: on_disk.to_string_lossy().into_owned(),
        directory: true,
        content: None,
    });
    second.inputs.push(Input {
        path: "made".into(),
        source: String::new(),
        directory: true,
        content: None,
    });
    second.stdin = Some("text\n".into());
    let request = request(&runner, &second);
    let kinds: Vec<FileKind> = request.spec.inputs.iter().map(|input| input.kind).collect();
    assert_eq!(
        kinds,
        [FileKind::File, FileKind::Directory, FileKind::Directory]
    );
    assert_eq!(request.spec.inputs[1].digest, db.digest);
    runner.execute(&request).expect("runs");
    let staged = runner.executor().staged.borrow();
    let staging = staged.last().expect("a staging");
    let files: Vec<&str> = staging
        .inputs
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(files, ["input.txt", "in/a.txt"]);
    assert_eq!(staging.directories, ["in", "in/empty", "made"]);
    let stdin = staging.stdin.as_ref().expect("a standard input");
    assert_eq!(std::fs::read_to_string(stdin).expect("reads"), "text\n");
}

#[test]
fn remote_inputs_are_not_supported_yet() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut remote = task(root.path(), "a");
    remote.inputs[0].source = "s3://bucket/input.txt".into();
    let error = runner.request(&remote, None).unwrap_err();
    assert!(error.unsupported(), "{error}");
}
