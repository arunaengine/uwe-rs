//! Runs small CWL documents through the driver and the mock TES executor without a job file,
//! so on placeholder inputs, as `iunctus run` and the graph corpus test do.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Launch, MockTes, execute};

/// Writes the files into a temporary folder, runs `main.cwl` and gives the task count or the
/// error.
fn run(files: &[(&str, &str)]) -> Result<usize, String> {
    let folder = tempfile::tempdir().map_err(|error| error.to_string())?;
    for (name, text) in files {
        std::fs::write(folder.path().join(name), text).map_err(|error| error.to_string())?;
    }
    let main = folder.path().join("main.cwl");
    let launch = Launch {
        target: main.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).map_err(|error| error.message)?;
    let mut executor = MockTes::new(Path::new(&launch.scratch).join("mock"));
    let execution = execute(&main.to_string_lossy(), &mut driver, &mut executor);
    match execution.error {
        Some(error) => Err(error.to_string()),
        None => Ok(execution.graph.nodes.len()),
    }
}

#[test]
fn outputs_that_only_cwl_output_json_gives_keep_their_format() {
    let make = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: make\ninputs: []\n\
        outputs:\n  made: {type: File, format: 'http://example.com/movies'}\n";
    let take = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: take\n\
        inputs:\n  movies: {type: File, format: 'http://example.com/movies', inputBinding: {}}\n\
        outputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\nsteps:\n  \
        make: {run: make.cwl, in: [], out: [made]}\n  \
        take: {run: take.cwl, in: {movies: make/made}, out: []}\n";
    let files = [("make.cwl", make), ("take.cwl", take), ("main.cwl", main)];
    assert_eq!(run(&files), Ok(2));
}

#[test]
fn placeholder_files_take_the_format_a_tool_asks_for() {
    let take = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: take\n\
        inputs:\n  reads: {type: File, format: 'http://example.com/fastq', inputBinding: {}}\n\
        outputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: {reads: File}\noutputs: []\n\
        steps:\n  take: {run: take.cwl, in: {reads: reads}, out: []}\n";
    assert_eq!(run(&[("take.cwl", take), ("main.cwl", main)]), Ok(1));
}

#[test]
fn rejects_negative_time_limits() {
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: [sleep, '1']\n\
        requirements: {ToolTimeLimit: {timelimit: -1}}\ninputs: []\noutputs: []\n";
    let error = run(&[("main.cwl", tool)]).expect_err("a negative time limit fails");
    assert!(error.contains("timelimit"), "{error}");
}

#[test]
fn absolute_entry_names_need_docker_as_a_requirement() {
    let tool = |docker: &str| {
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n{docker}\
             requirements:\n  InitialWorkDirRequirement:\n    listing:\n      \
             - {{entryname: /data/x.txt, entry: text}}\ninputs: []\noutputs: []\n"
        )
    };
    let hinted = tool("hints: {DockerRequirement: {dockerPull: alpine}}\n");
    let error = run(&[("main.cwl", &hinted)]).expect_err("a hint is not enough");
    assert!(error.contains("DockerRequirement"), "{error}");
    let required = tool("").replace(
        "requirements:\n",
        "requirements:\n  DockerRequirement: {dockerPull: alpine}\n",
    );
    assert_eq!(run(&[("main.cwl", &required)]), Ok(1));
}
