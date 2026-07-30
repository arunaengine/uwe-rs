//! Runs each workflow of `tests/fixtures/workflows` with the simulator and compares the
//! graph and the tasks with the folder's `expected.txt`. A folder may have a `targets` file
//! with one target per line, and a `snakemake.txt` with the shell commands that
//! `snakemake --dry-run --printshellcmds` (Snakemake 9.27.0) prints for it, which the tasks
//! must run. `UPDATE_EXPECTED=1` rewrites the expected files.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::path::Path;

use workflow_graph::{Launch, Simulator, execute};

/// Copies a folder with its files and folders.
fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("makes the folder");
    for entry in std::fs::read_dir(from).expect("reads the folder") {
        let entry = entry.expect("an entry");
        let target = to.join(entry.file_name());
        match entry.file_type().expect("a type").is_dir() {
            true => copy(&entry.path(), &target),
            false => {
                std::fs::copy(entry.path(), &target).expect("copies");
            }
        }
    }
}

/// The graph of a run, and for each task its image, command, inputs and outputs. The run is of
/// a copy of the folder outside the repository, so that no file that git tracks becomes a
/// source of its jobs, wherever the repository is.
fn describe(fixture: &Path) -> String {
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let folder = &scratch.path().join("workflow");
    copy(fixture, folder);
    let targets = std::fs::read_to_string(folder.join("targets"))
        .map(|text| {
            text.lines()
                .map(str::to_owned)
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let launch = Launch {
        target: folder.to_path_buf(),
        targets,
        scratch: scratch.path().to_path_buf(),
        simulated: true,
        ..Launch::default()
    };
    let mut text = String::new();
    let mut driver = match snakemake_runtime::driver(&launch) {
        Ok(driver) => driver,
        Err(error) => return format!("error: {error}\n"),
    };
    let mut executor = Simulator::new(scratch.path().join("simulator"));
    let execution = execute(&folder.to_string_lossy(), driver.as_mut(), &mut executor);
    text.push_str(&execution.graph.summary());
    for node in &execution.graph.nodes {
        let task = &node.task;
        let _ = writeln!(
            text,
            "task {}: {}",
            task.id,
            task.image.as_deref().unwrap_or("-")
        );
        let _ = writeln!(text, "  command: {}", task.command.join(" "));
        let inputs: Vec<&str> = task
            .inputs
            .iter()
            .map(|input| input.path.as_str())
            .collect();
        let _ = writeln!(text, "  inputs: {}", inputs.join(" "));
        let _ = writeln!(text, "  outputs: {}", task.outputs.join(" "));
    }
    if let Some(error) = execution.error {
        let prefix = format!("{}/", folder.display());
        let _ = writeln!(text, "error: {}", error.to_string().replace(&prefix, ""));
    }
    // Scripts see their folder at its path on this machine.
    text.replace(&folder.display().to_string(), "{folder}")
}

#[test]
fn workflows_give_the_expected_graphs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workflows");
    let mut folders: Vec<_> = std::fs::read_dir(&root)
        .expect("reads the fixtures")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    let update = std::env::var_os("UPDATE_EXPECTED").is_some();
    let mut failures = Vec::new();
    for folder in folders {
        let actual = describe(&folder);
        let path = folder.join("expected.txt");
        if update {
            std::fs::write(&path, &actual).expect("writes the expected graph");
            continue;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        if actual != expected {
            failures.push(format!("{}:\n{actual}", folder.display()));
        }
        let reference = std::fs::read_to_string(folder.join("snakemake.txt")).unwrap_or_default();
        for command in reference
            .lines()
            .filter_map(|line| line.strip_prefix("Shell command: "))
        {
            if command != "None" && !actual.contains(&format!("pipefail;  {command}\n")) {
                failures.push(format!("{}: no task runs `{command}`", folder.display()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "graphs differ:\n{}",
        failures.join("\n")
    );
}
