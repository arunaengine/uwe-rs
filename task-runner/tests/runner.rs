//! Checks the cache and staging of the runner with a fake executor.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use content_store::Store;
use executor::manifest::Collected;
use executor::{Executor, ExecutorError, Finished, Staging};
use task_model::{
    FORMAT, FileKind, Image, InputFile, OutputRule, Placement, ReuseScope, TaskRequest, TaskSpec,
};
use task_runner::Runner;

/// Writes each output pattern as a file with the staged input's content, prints `said`, and
/// remembers each run.
struct Fake {
    scratch: std::path::PathBuf,
    runs: RefCell<Vec<String>>,
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
        let mut finished = Finished {
            exit_code: Some(self.exit_code),
            files: Vec::new(),
            root: self.scratch.join(name),
        };
        std::fs::create_dir_all(finished.root.join("work"))?;
        std::fs::create_dir_all(finished.root.join("control"))?;
        std::fs::write(finished.stdout(), "said\n")?;
        let input = match staging.files.first() {
            Some(file) => std::fs::read_to_string(&file.source)?,
            None => String::new(),
        };
        for rule in &spec.outputs {
            std::fs::write(finished.local_path(&rule.pattern), &input)?;
            finished.files.push(Collected {
                path: rule.pattern.clone(),
                size: input.len() as u64,
            });
        }
        Ok(finished)
    }
}

fn request(runner: &Runner<Fake>, root: &Path, command: &str) -> TaskRequest {
    let input = root.join("input.txt");
    std::fs::write(&input, "data\n").expect("writes the input");
    let (digest, _) = runner.import(&input).expect("imports");
    TaskRequest {
        identity: "P (1)".into(),
        display: "P (1)".into(),
        scope: ReuseScope::Shared,
        attempt: 1,
        spec: TaskSpec {
            format: FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "debian@sha256:00".into(),
                platform: None,
            }),
            arguments: vec!["/bin/bash".into(), "-c".into(), command.into()],
            environment: BTreeMap::new(),
            inputs: vec![InputFile {
                path: "input.txt".into(),
                kind: FileKind::File,
                digest,
                executable: false,
                writable: false,
            }],
            outputs: vec![OutputRule {
                pattern: "out.txt".into(),
            }],
            stdin: None,
        },
        placement: Placement::default(),
    }
}

fn runner(root: &Path, exit_code: i32) -> Runner<Fake> {
    let store = Store::local(&root.join("store")).expect("a store");
    let fake = Fake {
        scratch: root.join("scratch"),
        runs: RefCell::new(Vec::new()),
        exit_code,
    };
    Runner::new(store, fake, "run1", &root.join("downloads")).expect("a runner")
}

#[test]
fn reuses_shared_results() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let first = runner
        .execute(&request(&runner, root.path(), "a"))
        .expect("runs");
    assert!(!first.reused);
    let [output] = first.files.as_slice() else {
        panic!("one output")
    };
    assert_eq!(output.location, "runs/run1/P__1_/1/work/out.txt");
    let stored = runner.local_path(&output.location).expect("a local store");
    assert_eq!(std::fs::read_to_string(stored).expect("reads"), "data\n");
    let said = runner
        .read_content(first.stdout.as_ref().expect("a standard output"))
        .expect("reads");
    assert_eq!(said, "said\n");
    let second = runner
        .execute(&request(&runner, root.path(), "a"))
        .expect("runs");
    assert!(second.reused);
    assert_eq!(second.files, first.files);
    assert_eq!(second.stdout, first.stdout);
    let third = runner
        .execute(&request(&runner, root.path(), "b"))
        .expect("runs");
    assert!(!third.reused);
    assert_eq!(runner.executor().runs.borrow().len(), 2);
}

#[test]
fn failures_are_not_cached() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 3);
    let first = runner
        .execute(&request(&runner, root.path(), "a"))
        .expect("runs");
    assert_eq!(first.exit_code, Some(3));
    let second = runner
        .execute(&request(&runner, root.path(), "a"))
        .expect("runs");
    assert!(!second.reused);
}
