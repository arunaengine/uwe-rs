//! Output JSON follows the files visible to the task, including staged directories.
// Copyright (c) 2026 The uwe Contributors
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
            attempt: 2,
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
fn reads_staged_children() {
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
        assert!(!driver.take_acceptance(task.id, 1));
        assert!(driver.take_acceptance(task.id, 2));
        assert!(!driver.take_acceptance(task.id, 2));
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
fn rejects_unrelated_locations() {
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
        assert!(!driver.take_acceptance(task.id, 2));
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
fn preserves_json_errors() {
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
            attempt: 2,
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
    assert!(!driver.take_acceptance(task.id, 2));
    let error = driver.finish().expect_err("read failure").message;
    assert!(error.contains("cannot read cwl.output.json"), "{error}");
}

#[test]
fn copies_directory_arrays() {
    for version in ["v1.1", "v1.2"] {
        let folder = tempfile::tempdir().expect("folder");
        let root = folder.path();
        std::fs::write(root.join("source.txt"), "staged bytes").expect("source");
        let mut directories = Vec::new();
        let work = root.join("work");
        let mut files = Vec::new();
        for name in ["testdir", "rec"] {
            let source = root.join(name);
            std::fs::create_dir(&source).expect("source directory");
            std::os::unix::fs::symlink(root.join("source.txt"), source.join("a"))
                .expect("source link");
            let copy = work.join(name).join("a");
            std::fs::create_dir_all(copy.parent().expect("parent")).expect("work directory");
            std::fs::copy(source.join("a"), &copy).expect("staged copy");
            directories.push(serde_json::json!({"class": "Directory",
                "location": cwl_parser::iri::file_url(&source)}));
            files.push(OutputFile {
                path: format!("{name}/a"),
                location: copy.to_string_lossy().into_owned(),
                ..OutputFile::default()
            });
        }
        std::fs::write(
            root.join("main.cwl"),
            format!(
                "cwlVersion: {version}\nclass: CommandLineTool\nbaseCommand: ls\n\
                 requirements:\n  InitialWorkDirRequirement:\n    listing: [$(inputs.data)]\n\
                 inputs: {{data: 'Directory[]'}}\noutputs:\n  out:\n    type: 'File[]'\n    \
                 outputBinding: {{glob: [testdir/a, rec/a]}}\n"
            ),
        )
        .expect("tool");
        std::fs::write(
            root.join("job.json"),
            serde_json::json!({"data": directories}).to_string(),
        )
        .expect("job");
        let mut driver = CwlDriver::new(
            &Launch {
                target: root.join("main.cwl"),
                inputs: Some(root.join("job.json")),
                scratch: root.join("scratch"),
                ..Launch::default()
            },
            Options {
                simulated: false,
                ..Options::default()
            },
        )
        .expect("driver");
        let task = driver.start().expect("starts").remove(0);
        for name in ["testdir", "rec"] {
            assert!(task.inputs.iter().any(|input| input.path == name));
        }
        driver
            .complete(Outcome {
                id: task.id,
                files,
                ..Outcome::default()
            })
            .expect("records outcome");
        driver.finish().expect("collects staged directories");
        let output = driver.outputs().expect("outputs")["out"]
            .as_list()
            .expect("files");
        assert_eq!(output.len(), 2);
        for file in output {
            let location = file.text("location").expect("location");
            assert!(
                cwl_parser::iri::file_path(location)
                    .expect("local")
                    .starts_with(&work)
            );
            assert_eq!(file.get("size"), Some(&cwl_runtime::Value::Integer(12)));
            assert!(file.get("checksum").is_some());
        }
    }
}

#[test]
fn rejects_record_escapes() {
    for simulated in [true, false] {
        for suffix in ["", "/..", "/%2e%2e", "-other"] {
            let prefix = if suffix.contains('%') { "file://" } else { "" };
            let folder = tempfile::tempdir().expect("folder");
            let root = folder.path();
            std::fs::write(
                root.join("main.cwl"),
                format!(
                    "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: 'true'\n\
                     requirements: {{InlineJavascriptRequirement: {{}}}}\ninputs: []\n\
                     outputs:\n  references:\n    type:\n      type: record\n      name: References\n      \
                     fields: {{genome: File, annotation: File}}\n    outputBinding:\n      \
                     outputEval: '$({{genome: {{class: \"File\", path: \"{prefix}\" + runtime.outdir + \"{suffix}/genome.fa\"}}, \
                     annotation: {{class: \"File\", path: \"{prefix}\" + runtime.outdir + \"{suffix}/annotation.gtf\"}}}})'\n"
                ),
            )
            .expect("tool");
            let mut driver = CwlDriver::new(
                &Launch {
                    target: root.join("main.cwl"),
                    scratch: root.join("scratch"),
                    ..Launch::default()
                },
                Options {
                    simulated,
                    ..Options::default()
                },
            )
            .expect("driver");
            let task = driver.start().expect("starts").remove(0);
            let mut files = Vec::new();
            let work = root.join("work");
            if !simulated {
                std::fs::create_dir(&work).expect("work directory");
                for name in ["genome.fa", "annotation.gtf"] {
                    let path = work.join(name);
                    std::fs::write(&path, "record output").expect("file");
                    files.push(OutputFile {
                        path: name.into(),
                        location: path.to_string_lossy().into_owned(),
                        ..OutputFile::default()
                    });
                }
            }
            driver
                .complete(Outcome {
                    id: task.id,
                    files,
                    ..Outcome::default()
                })
                .expect("records outcome");
            if !suffix.is_empty() {
                let error = driver.finish().expect_err("rejects escape").message;
                assert!(error.contains("outside the task's"), "{error}");
                continue;
            }
            driver.finish().expect("collects record output");
            let record = &driver.outputs().expect("outputs")["references"];
            for (field, name) in [("genome", "genome.fa"), ("annotation", "annotation.gtf")] {
                let file = record.get(field).expect("field");
                assert_eq!(file.text("basename"), Some(name));
                let expected = if simulated {
                    format!("_:{name}")
                } else {
                    cwl_parser::iri::file_url(&work.join(name))
                };
                assert_eq!(file.text("location"), Some(expected.as_str()));
                assert!(file.get("checksum").is_some());
            }
        }
    }
}
