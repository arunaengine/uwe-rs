//! Exports a small simulated run and checks its crate.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_crate::{METADATA, Options, export};
use run_database::{Database, RunStart};
use serde_json::Value;
use workflow_graph::{Edge, EdgeKind, Input, Language, Outcome, OutputFile, Task, TaskId};

const WORKFLOW: &str = "process UPPER {\n    input:\n    path text\n    output:\n    \
    path 'upper.txt'\n    script:\n    \"tr a-z A-Z < $text > upper.txt\"\n}\n";

fn task(id: u64, step: &str, inputs: Vec<Input>, output: &str) -> Task {
    Task {
        id: TaskId(id),
        name: format!("{step} ({id})"),
        step: step.into(),
        image: Some("debian:stable-slim".into()),
        command: vec!["/bin/bash".into(), "-ue".into(), ".command.sh".into()],
        script: Some(format!("tr a-z A-Z < in.txt > {output}")),
        environment: [("LC_ALL".to_owned(), "C".to_owned())].into(),
        inputs,
        outputs: vec![output.into()],
        ..Task::default()
    }
}

fn input(path: &str, source: &Path) -> Input {
    Input {
        path: path.into(),
        source: source.to_string_lossy().into_owned(),
        directory: false,
        content: None,
    }
}

fn finished(id: u64, path: &str, location: PathBuf) -> Outcome {
    Outcome {
        id: TaskId(id),
        exit_code: 0,
        files: vec![OutputFile {
            path: path.into(),
            location,
            directory: false,
        }],
        ..Outcome::default()
    }
}

/// Records a simulated run of two tasks in `directory`: UPPER reads `greeting.txt` and an
/// assumed input that does not exist, COUNT reads the output of UPPER.
fn record(directory: &Path) -> Database {
    let greeting = directory.join("greeting.txt");
    std::fs::write(&greeting, "hello\n").expect("writes the input");
    let work = directory.join("work");
    std::fs::create_dir_all(&work).expect("makes the work folder");
    let upper = work.join("upper.txt");
    std::fs::write(&upper, "HELLO\n").expect("writes an output");
    let count = work.join("count.txt");
    std::fs::write(&count, "1\n").expect("writes an output");
    let path = directory.join("runs.sqlite");
    let start = RunStart {
        id: "fixture-run".into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: Some("4f3c2a1".into()),
        arguments: vec![
            "run".into(),
            "main.nf".into(),
            "--greeting".into(),
            "hello".into(),
        ],
        parameters: vec![("greeting".into(), "hello".into())],
        executor: "simulator".into(),
        simulated: true,
        store: None,
    };
    let mut recorder = Database::open(&path)
        .expect("opens")
        .record(&start)
        .expect("starts");
    let assumed = input("reference.fa", Path::new("/data/reference.fa"));
    let first = task(
        1,
        "UPPER",
        vec![input("in.txt", &greeting), assumed],
        "upper.txt",
    );
    recorder.requested(&first, &[]);
    recorder.started(TaskId(1), 1);
    recorder.finished(&finished(1, "upper.txt", upper.clone()), 1, false);
    let second = task(2, "COUNT", vec![input("in.txt", &upper)], "count.txt");
    let edge = Edge {
        from: TaskId(1),
        to: TaskId(2),
        kind: EdgeKind::File {
            path: "in.txt".into(),
        },
    };
    recorder.requested(&second, &[edge]);
    recorder.started(TaskId(2), 1);
    recorder.finished(&finished(2, "count.txt", count), 1, false);
    recorder.ended(None);
    recorder.close().expect("closes");
    Database::open(&path).expect("opens again")
}

/// The metadata with every time replaced, since times differ on each run.
fn normalized(metadata: &str) -> Value {
    let mut value: Value = serde_json::from_str(metadata).expect("valid JSON");
    if let Some(graph) = value["@graph"].as_array_mut() {
        for entity in graph {
            for key in ["startTime", "endTime", "datePublished"] {
                if entity.get(key).is_some() {
                    entity[key] = Value::from("<time>");
                }
            }
        }
    }
    value
}

fn entity<'a>(metadata: &'a Value, id: &str) -> &'a Value {
    metadata["@graph"]
        .as_array()
        .and_then(|graph| graph.iter().find(|entity| entity["@id"] == id))
        .unwrap_or_else(|| panic!("no entity {id}"))
}

#[test]
fn a_simulated_run_becomes_a_provenance_run_crate() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let database = record(directory.path());
    let workflow = directory.path().join("main.nf");
    std::fs::write(&workflow, WORKFLOW).expect("writes the workflow");
    let output = directory.path().join("crate");
    let options = Options {
        workflow,
        license: None,
    };
    let summary = export(&database, "fixture-run", &output, &options).expect("exports");
    // The input, the two outputs; the assumed input does not exist.
    assert_eq!((summary.tasks, summary.files, summary.bytes), (2, 3, 14));

    let written = std::fs::read_to_string(output.join(METADATA)).expect("reads the metadata");
    let metadata = normalized(&written);
    let root = entity(&metadata, "./");
    assert_eq!(root["mainEntity"]["@id"], "main.nf");
    assert!(
        root["description"]
            .as_str()
            .is_some_and(|text| text.starts_with("Simulated run"))
    );
    assert_eq!(root["additionalProperty"]["@id"], "#simulated");
    assert_eq!(entity(&metadata, "#simulated")["value"], "true");
    let upper = entity(&metadata, "#task/1");
    assert_eq!(upper["@type"], "CreateAction");
    assert_eq!(upper["instrument"]["@id"], "#tool/UPPER");
    assert!(
        upper["description"]
            .as_str()
            .is_some_and(|text| text.starts_with("Simulated"))
    );
    let hello = format!("data/{}", task_model::Digest::sha256(b"HELLO\n").hex());
    assert_eq!(upper["result"]["@id"], hello.as_str());
    assert_eq!(
        entity(&metadata, "#task/2")["object"]["@id"],
        hello.as_str()
    );
    assert_eq!(
        std::fs::read(output.join(&hello)).expect("reads"),
        b"HELLO\n"
    );
    let image = entity(&metadata, "#image/0");
    assert_eq!(
        (&image["name"], &image["tag"]),
        (&Value::from("library/debian"), &Value::from("stable-slim"))
    );
    let run = entity(&metadata, "#run/fixture-run");
    // The workflow reads the greeting, the assumed reference and a parameter, and makes the
    // count, which no task reads.
    assert_eq!(run["object"].as_array().map(Vec::len), Some(3));
    let count = format!("data/{}", task_model::Digest::sha256(b"1\n").hex());
    assert_eq!(run["result"]["@id"], count.as_str());
}
