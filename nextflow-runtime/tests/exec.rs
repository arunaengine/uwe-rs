//! Processes with `exec:` sections run in the interpreter when their task is asked for: the
//! task has no image and no command, its files are those the section made in `task.workDir`,
//! and the tasks that read them wait for it (tests/fixtures/exec, recorded with Nextflow
//! 26.04.6).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::{Launch, Simulator, execute};

/// The files below a folder, by their paths there.
fn files(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn interprets_exec_sections() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exec");
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::copy(fixture.join("main.nf"), directory.path().join("main.nf")).expect("copies");
    let scratch = directory.path().join("scratch");
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: scratch.clone(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(directory.path().join("simulator"));
    let graph = execute("main.nf", driver.as_mut(), &mut executor)
        .into_result()
        .expect("the run succeeds");
    graph.check().expect("a valid graph");
    let tasks: Vec<_> = graph.nodes.iter().map(|node| &node.task).collect();
    let steps = |step: &'static str| tasks.iter().filter(move |task| task.step == step);
    assert_eq!(steps("MAKE_SHEET").count(), 2);
    for task in steps("MAKE_SHEET").chain(steps("CHECK")) {
        assert!(task.image.is_none() && task.command.is_empty(), "{task:?}");
    }
    // A task that reads what an `exec:` section made waits for it.
    for task in steps("COUNT").chain(steps("CHECK")) {
        assert_eq!(task.after.len(), 1, "{task:?}");
        let maker = tasks.iter().find(|other| other.id == task.after[0]);
        assert_eq!(maker.map(|maker| maker.step.as_str()), Some("MAKE_SHEET"));
    }
    // The sections made the files that Nextflow publishes.
    let made = files(&scratch.join("nextflow-exec"));
    for published in [
        "sheets/A.csv",
        "sheets/BB.csv",
        "checks/A.ok",
        "checks/BB.ok",
    ] {
        let recorded = fixture.join("nextflow-26.04.6/results").join(published);
        let name = recorded.file_name().expect("a name");
        let file = (made.iter())
            .find(|path| path.file_name() == Some(name))
            .unwrap_or_else(|| panic!("no {published} made"));
        assert_eq!(
            std::fs::read_to_string(file).expect("reads"),
            std::fs::read_to_string(&recorded).expect("reads the record"),
            "{published}"
        );
    }
}
