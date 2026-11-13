//! Job files that name a File or Directory by a plain text, as the input schema describes it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options};
use serde_json::{Value, json};
use workflow_graph::{Driver, Launch, Simulator, execute};

const TOOL: &str = "cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs:
  reads: File
  lanes: 'File[]'
  extra: 'Directory?'
outputs: []
";

/// A folder with the tool, two files and a folder, and a job file below `jobs/`.
fn folder(job: &Value) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("folder");
    std::fs::create_dir_all(root.path().join("jobs/data/tree")).expect("folders");
    std::fs::write(root.path().join("main.cwl"), TOOL).expect("tool");
    for name in ["jobs/a.txt", "jobs/b.txt"] {
        std::fs::write(root.path().join(name), "text").expect("file");
    }
    std::fs::write(root.path().join("jobs/job.json"), job.to_string()).expect("job");
    root
}

fn launch(root: &Path) -> Launch {
    Launch {
        target: root.join("main.cwl"),
        inputs: Some(root.join("jobs/job.json")),
        scratch: root.join("scratch"),
        ..Launch::default()
    }
}

fn driver(root: &Path) -> CwlDriver {
    CwlDriver::new(&launch(root), Options::default()).expect("driver")
}

/// Runs the job on the simulator and gives the error, if any.
fn run(root: &Path) -> Option<String> {
    let mut driver = driver(root);
    let mut executor = Simulator::new(root.join("scratch/simulator"));
    let execution = execute("main.cwl", &mut driver, &mut executor);
    execution.error.map(|error| error.to_string())
}

fn url(root: &Path, name: &str) -> String {
    cwl_parser::iri::file_url(&root.join(name))
}

fn validate(root: &Path) -> Vec<String> {
    let inputs = driver(root).inputs().expect("describes").expect("inputs");
    let validation = input_schema::validate(&inputs, false);
    validation
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect()
}

#[test]
fn runs_plain_texts() {
    let root = folder(&json!({
        "reads": "a.txt", "lanes": ["a.txt", "b.txt"], "extra": "data"
    }));
    assert_eq!(run(root.path()), None);
    assert_eq!(validate(root.path()), Vec::<String>::new());
}

#[test]
fn runs_file_objects() {
    let root = folder(&json!({
        "reads": {"class": "File", "path": "a.txt"},
        "lanes": [{"class": "File", "location": "b.txt"}],
        "extra": {"class": "Directory", "path": "data"}
    }));
    assert_eq!(run(root.path()), None);
    assert_eq!(validate(root.path()), Vec::<String>::new());
}

#[test]
fn runs_mixed_forms() {
    let root = folder(&json!({
        "reads": {"class": "File", "path": "a.txt"},
        "lanes": ["a.txt", {"class": "File", "path": "b.txt"}]
    }));
    assert_eq!(run(root.path()), None);
    assert_eq!(validate(root.path()), Vec::<String>::new());
}

#[test]
fn resolves_relative_texts() {
    let root = folder(&json!({
        "reads": "a.txt", "lanes": ["b.txt"], "extra": "data"
    }));
    let object = driver(root.path()).input_object().expect("job");
    let location = |value: &cwl_runtime::Value| value.text("location").map(str::to_owned);
    // Relative to the job file, as a relative `path` is.
    assert_eq!(
        location(&object["reads"]),
        Some(url(root.path(), "jobs/a.txt"))
    );
    assert_eq!(object["reads"].text("class"), Some("File"));
    let lanes = object["lanes"].as_list().expect("a list");
    assert_eq!(location(&lanes[0]), Some(url(root.path(), "jobs/b.txt")));
    assert_eq!(object["extra"].text("class"), Some("Directory"));
    assert_eq!(
        location(&object["extra"]),
        Some(url(root.path(), "jobs/data"))
    );
}

#[test]
fn keeps_addresses() {
    let root = folder(&json!({
        "reads": "https://example.org/a.txt", "lanes": ["s3://bucket/b.txt"]
    }));
    let object = driver(root.path()).input_object().expect("job");
    assert_eq!(
        object["reads"].text("location"),
        Some("https://example.org/a.txt")
    );
    let lanes = object["lanes"].as_list().expect("a list");
    assert_eq!(lanes[0].text("location"), Some("s3://bucket/b.txt"));
}

#[test]
fn reports_missing_texts() {
    let root = folder(&json!({"reads": "missing.txt", "lanes": []}));
    let options = Options {
        simulated: false,
        ..Options::default()
    };
    let mut driver = CwlDriver::new(&launch(root.path()), options).expect("driver");
    let error = driver.start().expect_err("a missing file fails");
    assert!(error.message.contains("missing.txt"), "{}", error.message);
}

#[test]
fn reads_nested_texts() {
    let root = folder(&json!({
        "sample": {"name": "a.txt", "reads": "a.txt", "mate": "b.txt"},
        "either": "a.txt"
    }));
    std::fs::write(
        root.path().join("main.cwl"),
        "cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs:
  sample:
    type:
      type: record
      fields:
        name: string
        reads: File
        mate: 'File?'
  either: [string, File]
outputs: []
",
    )
    .expect("tool");
    let object = driver(root.path()).input_object().expect("job");
    let sample = object["sample"].as_map().expect("a record");
    // A text stays a text where the type takes a string.
    assert_eq!(sample["name"].as_str(), Some("a.txt"));
    assert_eq!(sample["reads"].text("class"), Some("File"));
    assert_eq!(
        sample["mate"].text("location"),
        Some(url(root.path(), "jobs/b.txt").as_str())
    );
    assert_eq!(object["either"].as_str(), Some("a.txt"));
    assert_eq!(run(root.path()), None);
}
