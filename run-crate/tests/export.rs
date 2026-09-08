//! Exports a small simulated run and compares its crate with the fixture crate in
//! `tests/fixtures/simulated/`, which the Provenance Run Crate validator checked
//! (`tests/fixtures/validate.sh` writes the fixture again and records the validator's report in
//! `tests/fixtures/validation.txt`).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_crate::{METADATA, Options, export};
use run_database::{Database, RemoteFile, RunStart, Source, SourceKind};
use serde_json::Value;
use workflow_graph::{Edge, EdgeKind, Input, Language, Outcome, OutputFile, Task, TaskId};

const WORKFLOW: &str = "process UPPER {\n    input:\n    path text\n    output:\n    \
    path 'upper.txt'\n    script:\n    \"tr a-z A-Z < $text > upper.txt\"\n}\n";

/// A module of the workflow and a config file that the command line named, which the store of
/// the run keeps.
const MODULE: &str = "process COUNT {\n    script:\n    \"wc -l < in.txt > count.txt\"\n}\n";
const CONFIG: &str = "params.greeting = 'hello'\n";

/// The files that pin the run: the files of its workflow and its named config file.
fn sources() -> Vec<Source> {
    let source = |kind, path: &str, text: &str| Source {
        kind,
        path: path.into(),
        digest: task_model::Digest::sha256(text.as_bytes()).to_string(),
        size: text.len() as u64,
    };
    vec![
        source(SourceKind::Workflow, "main.nf", WORKFLOW),
        source(SourceKind::Workflow, "modules/count.nf", MODULE),
        source(SourceKind::Named, "/work/extra.config", CONFIG),
    ]
}

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
        ..Default::default()
    }
}

fn finished(id: u64, path: &str, location: PathBuf) -> Outcome {
    Outcome {
        id: TaskId(id),
        exit_code: 0,
        files: vec![OutputFile {
            path: path.into(),
            location: location.to_string_lossy().into_owned(),
            ..OutputFile::default()
        }],
        ..Outcome::default()
    }
}

/// Records a simulated run of two tasks in `directory`: UPPER reads `greeting.txt` and an
/// assumed input that does not exist, COUNT reads the output of UPPER.
fn record(directory: &Path) -> Database {
    record_with_address(directory, None)
}

fn record_with_address(directory: &Path, address: Option<&str>) -> Database {
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
        program: Some("iunctus 0.1.0".into()),
        launch: Some("/work".into()),
        workflow_folder: Some("/work".into()),
        repeats: None,
    };
    let mut recorder = Database::open(&path)
        .expect("opens")
        .record(&start)
        .expect("starts");
    let inputs = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["greeting"],
        "properties": {
            "greeting": {"type": "string", "description": "The text to shout."},
            "reference": {
                "type": "string",
                "format": "file-path",
                "exists": true,
                "mimetype": "text/x-fasta",
                "default": "/data/reference.fa"
            }
        }
    });
    let values = serde_json::json!({"greeting": "hello", "reference": "/data/reference.fa"});
    recorder.inputs(&inputs, &values);
    recorder.outputs(&serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "count": {"type": "string", "format": "file-path", "description": "The count."}
        }
    }));
    recorder.sources(&sources());
    recorder.remote_files(&[RemoteFile {
        address: "https://example.org/samples.csv".into(),
        digest: task_model::Digest::sha256(b"id\na\n").to_string(),
        size: 5,
        fetched: 1_790_000_000_000,
        version: Some("\"1\"".into()),
    }]);
    let mut assumed = input("reference.fa", Path::new("/data/reference.fa"));
    if let Some(address) = address {
        assumed.source = address.to_owned();
    }
    let mut first = task(
        1,
        "UPPER",
        vec![input("in.txt", &greeting), assumed],
        "upper.txt",
    );
    if let Some(address) = address {
        first.script = Some(format!("curl '{address}'"));
    }
    recorder.requested(&first, &[]);
    recorder.started(TaskId(1), 1);
    let mut first_outcome = finished(1, "upper.txt", upper.clone());
    first_outcome.inputs = std::collections::BTreeMap::from([(
        "in.txt".into(),
        task_model::Digest::sha256(b"hello\n").to_string(),
    )]);
    recorder.finished(&first_outcome, 1, false);
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
    let mut second_outcome = finished(2, "count.txt", count);
    second_outcome.inputs = std::collections::BTreeMap::from([(
        "in.txt".into(),
        task_model::Digest::sha256(b"HELLO\n").to_string(),
    )]);
    recorder.finished(&second_outcome, 1, false);
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
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simulated");
    let output = if std::env::var_os("RUN_CRATE_WRITE_FIXTURE").is_some() {
        let _ = std::fs::remove_dir_all(&fixture);
        fixture.clone()
    } else {
        directory.path().join("crate")
    };
    // The store keeps the files of the workflow and of the command line as the run had them.
    let kept = directory.path().join("kept");
    std::fs::create_dir_all(&kept).expect("makes a folder");
    let mut copies = std::collections::HashMap::new();
    for (source, text) in sources().iter().zip([WORKFLOW, MODULE, CONFIG]) {
        let copy = kept.join(&source.digest[7..]);
        std::fs::write(&copy, text).expect("writes a kept copy");
        copies.insert(source.digest.clone(), copy);
    }
    let options = Options {
        workflow,
        license: None,
        sources: copies,
    };
    let summary = export(&database, "fixture-run", &output, &options).expect("exports");
    // The input, the two outputs and the config file; the assumed input does not exist.
    let bytes = 14 + CONFIG.len() as u64;
    assert_eq!((summary.tasks, summary.files, summary.bytes), (2, 4, bytes));

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
    // The workflow reads the greeting, the assumed reference, a parameter, the config file and
    // the sample sheet on the web, and makes the count, which no task reads.
    assert_eq!(run["object"].as_array().map(Vec::len), Some(5));
    let count = format!("data/{}", task_model::Digest::sha256(b"1\n").hex());
    assert_eq!(run["result"]["@id"], count.as_str());
    // The inputs of the schema are formal parameters of the workflow, and the given parameter
    // is an example of its formal parameter.
    let workflow = entity(&metadata, "main.nf");
    assert_eq!(
        workflow["input"],
        serde_json::json!([{"@id": "#input/greeting"}, {"@id": "#input/reference"}])
    );
    let greeting = entity(&metadata, "#input/greeting");
    assert_eq!(
        (&greeting["additionalType"], &greeting["valueRequired"]),
        (&Value::from("Text"), &Value::from(true))
    );
    let reference = entity(&metadata, "#input/reference");
    assert_eq!(reference["additionalType"], "File");
    assert_eq!(reference["encodingFormat"], "text/x-fasta");
    assert_eq!(
        entity(&metadata, "#absent//data/reference.fa")["exampleOfWork"]["@id"],
        "#input/reference"
    );
    assert_eq!(
        entity(&metadata, "#parameter/greeting")["exampleOfWork"]["@id"],
        "#input/greeting"
    );
    assert_eq!(
        entity(&metadata, "inputs.schema.json")["encodingFormat"],
        "application/schema+json"
    );
    assert!(output.join("inputs.schema.json").is_file());
    // What pins the run: the files of the workflow as it ran, the named config file, the remote
    // file that the workflow read, and the schema of the outputs.
    assert_eq!(
        std::fs::read_to_string(output.join("workflow/modules/count.nf")).expect("reads"),
        MODULE
    );
    let folder = entity(&metadata, "workflow/");
    assert_eq!(folder["hasPart"].as_array().map(Vec::len), Some(2));
    let config = format!(
        "data/{}",
        task_model::Digest::sha256(CONFIG.as_bytes()).hex()
    );
    assert_eq!(
        entity(&metadata, &config)["alternateName"],
        "/work/extra.config"
    );
    let samples = entity(&metadata, "https://example.org/samples.csv");
    assert_eq!(samples["sdDatePublished"], "2026-09-21T14:13:20.000+00:00");
    let objects = run["object"].as_array().expect("objects");
    for id in [config.as_str(), "https://example.org/samples.csv"] {
        assert!(objects.iter().any(|object| object["@id"] == id), "{id}");
    }
    assert_eq!(workflow["output"]["@id"], "#output/count");
    assert_eq!(entity(&metadata, "#output/count")["additionalType"], "File");
    assert!(output.join("outputs.schema.json").is_file());

    let recorded = std::fs::read_to_string(fixture.join(METADATA)).expect("reads the fixture");
    assert_eq!(
        metadata,
        normalized(&recorded),
        "the crate differs from the fixture; run run-crate/tests/fixtures/validate.sh to write \
         and check it again"
    );
}

#[test]
fn historical_workflow_exports_after_checkout_deletion_and_rejects_wrong_kept_bytes() {
    let root = tempfile::tempdir().expect("a directory");
    let database = record(root.path());
    let kept = root.path().join("kept.nf");
    std::fs::write(&kept, WORKFLOW).expect("writes kept workflow");
    let mut options = Options {
        workflow: PathBuf::from("/work/main.nf"),
        sources: [(sources()[0].digest.clone(), kept.clone())].into(),
        ..Options::default()
    };
    let output = root.path().join("crate");
    export(&database, "fixture-run", &output, &options).expect("exports deleted workflow");
    assert_eq!(
        std::fs::read_to_string(output.join("main.nf")).expect("reads"),
        WORKFLOW
    );
    std::fs::write(&kept, "replacement workflow").expect("replaces kept bytes");
    assert!(export(&database, "fixture-run", &root.path().join("bad"), &options).is_err());
    options.sources.clear();
    assert!(
        export(
            &database,
            "fixture-run",
            &root.path().join("missing"),
            &options
        )
        .is_err()
    );
}

#[test]
fn changed_inputs_export_only_kept_historical_bytes_or_record_unavailability() {
    let root = tempfile::tempdir().expect("a directory");
    let database = record(root.path());
    let workflow = root.path().join("main.nf");
    std::fs::write(&workflow, WORKFLOW).expect("writes workflow");
    std::fs::write(root.path().join("greeting.txt"), "replacement input").expect("replaces input");
    let digest = task_model::Digest::sha256(b"hello\n");
    let mut options = Options {
        workflow,
        ..Options::default()
    };
    let output = root.path().join("absent-crate");
    export(&database, "fixture-run", &output, &options).expect("exports unavailable input");
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(output.join(METADATA)).expect("reads metadata"))
            .expect("parses metadata");
    let absent = format!(
        "#absent/{}/{}",
        root.path().join("greeting.txt").to_string_lossy(),
        digest
    );
    assert_eq!(entity(&metadata, &absent)["sha256"], digest.hex());
    assert!(
        entity(&metadata, &absent)["description"]
            .as_str()
            .expect("description")
            .contains("unavailable")
    );
    assert!(
        !output
            .join("data")
            .join(task_model::Digest::sha256(b"replacement input").hex())
            .exists()
    );
    let kept = root.path().join("kept-input");
    std::fs::write(&kept, "hello\n").expect("writes kept input");
    options.sources.insert(digest.to_string(), kept);
    let output = root.path().join("kept-crate");
    export(&database, "fixture-run", &output, &options).expect("exports historical input");
    assert_eq!(
        std::fs::read(output.join("data").join(digest.hex())).expect("reads original"),
        b"hello\n"
    );
}

#[test]
fn crate_metadata_hides_url_passwords_while_private_replay_keeps_them() {
    let root = tempfile::tempdir().expect("a directory");
    let address = "ftp://user:sentinel-private-password@host?email=a:b@c#fragment@x";
    let database = record_with_address(root.path(), Some(address));
    let workflow = root.path().join("main.nf");
    std::fs::write(&workflow, WORKFLOW).expect("writes workflow");
    let output = root.path().join("crate");
    export(
        &database,
        "fixture-run",
        &output,
        &Options {
            workflow,
            ..Options::default()
        },
    )
    .expect("exports");
    let metadata = std::fs::read_to_string(output.join(METADATA)).expect("reads metadata");
    assert!(!metadata.contains("sentinel-private-password"));
    assert!(metadata.contains("ftp://user:[secret]@host?email=a:b@c#fragment@x"));
    let task = database
        .task("fixture-run", 1)
        .expect("reads task")
        .expect("a task");
    assert_eq!(task.description.inputs[1].source, address);
    assert!(
        task.description
            .script
            .expect("private command")
            .contains("sentinel-private-password")
    );
    assert_eq!(
        std::fs::read_to_string(output.join("main.nf")).expect("reads copied bytes"),
        WORKFLOW
    );
}
