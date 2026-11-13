//! Compares generated scripts with recorded Snakemake 9.27.0 preambles and normalized folders.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{Launch, Simulator, Task, execute};

/// The tasks of a workflow in a simulated run.
fn tasks(folder: &Path) -> Vec<Task> {
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let launch = Launch {
        target: folder.to_path_buf(),
        scratch: scratch.path().to_path_buf(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(scratch.path().join("simulator"));
    let execution = execute("preambles", driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    execution
        .graph
        .nodes
        .into_iter()
        .map(|node| node.task)
        .collect()
}

#[test]
fn matches_snakemake_preambles() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/embedded/preambles");
    let tasks = tasks(&folder);
    let shown = folder.to_string_lossy().into_owned();
    let mut compared = 0;
    for extension in ["R", "sh", "jl", "rs", "Rmd"] {
        let path = format!(".snakemake/scripts/test.{extension}");
        let script = tasks
            .iter()
            .flat_map(|task| &task.inputs)
            .find(|input| input.path == path)
            .and_then(|input| input.content.clone())
            .unwrap_or_else(|| panic!("no task has {path}"));
        let script = script.replace(&shown, "{folder}");
        let expected =
            std::fs::read_to_string(folder.join(format!("snakemake-9.27.0/test.{extension}")))
                .expect("a recorded script");
        assert_eq!(script, expected, "the script test.{extension} differs");
        compared += 1;
    }
    assert_eq!(compared, 5);
}
