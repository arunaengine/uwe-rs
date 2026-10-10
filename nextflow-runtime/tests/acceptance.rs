//! Native Nextflow output validation drives per-attempt acceptance.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

fn driver(strategy: &str) -> (Box<dyn Driver>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.nf"),
        "process P {\n output:\n path 'out.txt', arity: '1'\n script:\n 'true'\n}\nworkflow { P() }\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("nextflow.config"),
        format!("process.errorStrategy = '{strategy}'\nprocess.maxRetries = 1\n"),
    )
    .unwrap();
    let driver = nextflow_runtime::driver(&Launch {
        target: directory.path().to_owned(),
        ..Launch::default()
    })
    .unwrap();
    (driver, directory)
}

fn outcome(task: &Task, attempt: u32, exit_code: i32, file: bool, cached: bool) -> Outcome {
    Outcome {
        id: task.id,
        attempt,
        exit_code,
        cached,
        files: file
            .then(|| OutputFile {
                path: "out.txt".into(),
                location: "/work/out.txt".into(),
                ..OutputFile::default()
            })
            .into_iter()
            .collect(),
        ..Outcome::default()
    }
}

#[test]
fn accepts_exact_attempt() {
    let (mut driver, directory) = driver("terminate");
    let task = driver.start().unwrap().remove(0);
    driver.complete(outcome(&task, 1, 0, true, false)).unwrap();
    assert!(!driver.take_acceptance(task.id, 2));
    assert!(driver.take_acceptance(task.id, 1));
    assert!(!driver.take_acceptance(task.id, 1));
    let path = directory.path().to_owned();
    drop(driver);
    drop(directory);
    assert!(!path.exists());
}

#[test]
fn rejects_failed_acceptance() {
    for (exit_code, file) in [(0, false), (3, true)] {
        let (mut driver, _directory) = driver("ignore");
        let task = driver.start().unwrap().remove(0);
        driver
            .complete(outcome(&task, 1, exit_code, file, false))
            .unwrap();
        assert!(!driver.take_acceptance(task.id, 1));
    }
}

#[test]
fn isolates_retry_acceptance() {
    let (mut driver, _directory) = driver("retry");
    let first = driver.start().unwrap().remove(0);
    let second = driver
        .complete(outcome(&first, 1, 0, false, false))
        .unwrap()
        .remove(0);
    assert_eq!(second.id, first.id);
    assert!(!driver.take_acceptance(first.id, 1));
    driver
        .complete(outcome(&second, 2, 0, true, false))
        .unwrap();
    assert!(!driver.take_acceptance(second.id, 1));
    assert!(driver.take_acceptance(second.id, 2));
}

#[test]
fn revalidates_cached_outcomes() {
    for (file, accepted) in [(false, false), (true, true)] {
        let (mut driver, _directory) = driver(if accepted { "terminate" } else { "ignore" });
        let task = driver.start().unwrap().remove(0);
        driver.complete(outcome(&task, 1, 0, file, true)).unwrap();
        assert_eq!(driver.take_acceptance(task.id, 1), accepted);
    }
}
