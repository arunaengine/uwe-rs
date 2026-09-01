//! Output JSON follows the files visible to the task, including staged directories.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

fn start(root: &Path, input_type: &str, job: serde_json::Value) -> (CwlDriver, Task) {
    let tool = format!(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\n\
         inputs: {{data: {input_type}}}\noutputs: {{out: File}}\n"
    );
    std::fs::write(root.join("main.cwl"), tool).expect("tool");
    std::fs::write(root.join("job.json"), job.to_string()).expect("job");
    let launch = Launch {
        target: root.join("main.cwl"),
        inputs: Some(root.join("job.json")),
        scratch: root.join("scratch"),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(
        &launch,
        Options {
            simulated: false,
            ..Options::default()
        },
    )
    .expect("driver");
    let mut tasks = driver.start().expect("starts");
    assert_eq!(tasks.len(), 1);
    (driver, tasks.remove(0))
}

fn output(driver: &mut CwlDriver, task: &Task, root: &Path, value: serde_json::Value) {
    let work = root.join("work");
    std::fs::create_dir_all(&work).expect("work folder");
    let json = work.join("cwl.output.json");
    std::fs::write(&json, value.to_string()).expect("output JSON");
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![OutputFile {
                path: "cwl.output.json".into(),
                location: json.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .expect("records outcome");
}

#[test]
fn output_json_returns_staged_files_and_unlisted_directory_children() {
    for directory in [false, true] {
        let folder = tempfile::tempdir().expect("folder");
        let root = folder.path();
        let source = root.join(if directory { "input" } else { "input.txt" });
        if directory {
            std::fs::create_dir(&source).expect("input directory");
        }
        let leaf = if directory {
            source.join("child.txt")
        } else {
            source.clone()
        };
        std::fs::write(&leaf, "original bytes").expect("input file");
        let (mut driver, task) = start(
            root,
            if directory { "Directory" } else { "File" },
            serde_json::json!({"data": {"class": if directory { "Directory" } else { "File" },
                "location": cwl_parser::iri::file_url(&source)}}),
        );
        let staged = task
            .inputs
            .iter()
            .find(|input| input.source == source.to_string_lossy())
            .expect("staged input");
        let path = if directory {
            format!("{}/child.txt", staged.path)
        } else {
            staged.path.clone()
        };
        output(
            &mut driver,
            &task,
            root,
            serde_json::json!({"out": {"class": "File", "path": path}}),
        );
        let out = driver
            .outputs()
            .expect("output object")
            .get("out")
            .expect("file");
        assert_eq!(
            out.text("location"),
            Some(cwl_parser::iri::file_url(&leaf).as_str())
        );
        assert_eq!(out.get("size"), Some(&cwl_runtime::Value::Integer(14)));
        assert!(out.get("checksum").is_some());
        assert!(!driver.draining());
    }
}

#[test]
fn output_json_rejects_unrelated_and_parent_locations_before_reading() {
    for location in [
        "../unrelated.txt",
        "%2e%2e/unrelated.txt",
        "file:///unrelated-cwl-output.txt",
    ] {
        let folder = tempfile::tempdir().expect("folder");
        let source = folder.path().join("input.txt");
        std::fs::write(&source, "input").expect("input");
        let (mut driver, task) = start(
            folder.path(),
            "File",
            serde_json::json!({"data": {
            "class": "File", "location": cwl_parser::iri::file_url(&source)}}),
        );
        output(
            &mut driver,
            &task,
            folder.path(),
            serde_json::json!({"out": {"class": "File", "location": location}}),
        );
        assert!(driver.outputs().is_none());
        let error = driver.finish().expect_err("location rejected").message;
        assert!(error.contains("outside the task's"), "{error}");
        assert!(
            !error.contains("cannot read"),
            "mapping fails before checksum: {error}"
        );
    }
}

#[test]
fn missing_output_json_preserves_the_read_error() {
    let folder = tempfile::tempdir().expect("folder");
    let source = folder.path().join("input.txt");
    std::fs::write(&source, "input").expect("input");
    let (mut driver, task) = start(
        folder.path(),
        "File",
        serde_json::json!({"data": {
        "class": "File", "location": cwl_parser::iri::file_url(&source)}}),
    );
    driver
        .complete(Outcome {
            id: task.id,
            files: vec![OutputFile {
                path: "cwl.output.json".into(),
                location: folder
                    .path()
                    .join("work/cwl.output.json")
                    .to_string_lossy()
                    .into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        })
        .expect("records failure");
    let error = driver.finish().expect_err("read failure").message;
    assert!(error.contains("cannot read cwl.output.json"), "{error}");
}
