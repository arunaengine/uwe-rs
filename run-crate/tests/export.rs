//! Checks simulated and historical run exports against retained provenance fixtures.
// Copyright (c) 2026 The uwe Contributors
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
        executable: false,
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
        program: Some("uwe 0.1.0".into()),
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
fn exports_simulated_provenance() {
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
        ..Options::default()
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
fn exports_historical_workflows() {
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
fn preserves_historical_inputs() {
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
fn redacts_metadata_passwords() {
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

#[test]
fn hides_private_values() {
    let root = tempfile::tempdir().expect("a directory");
    let folder = root.path().join("server/run");
    let work = folder.join("work");
    std::fs::create_dir_all(&work).expect("makes the work folder");
    let secret = "sentinel-secret-value";
    let workflow = folder.join("main.nf");
    std::fs::write(&workflow, WORKFLOW).expect("writes the workflow");
    let config = format!("params.token = '{secret}'\n");
    let named = folder.join("secret.config");
    std::fs::write(&named, &config).expect("writes the config");
    let output = work.join("upper.txt");
    std::fs::write(&output, "HELLO\n").expect("writes an output");
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let start = RunStart {
        id: "secret-run".into(),
        resumes: None,
        workflow: text(&workflow),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into(), text(&workflow), "-c".into(), text(&named)],
        parameters: vec![],
        executor: "local".into(),
        simulated: false,
        store: None,
        program: Some("uwe 0.1.0".into()),
        launch: Some(text(&folder)),
        workflow_folder: Some(text(&folder)),
        repeats: None,
    };
    let path = root.path().join("runs.sqlite");
    let mut recorder = (Database::open(&path).expect("opens"))
        .record(&start)
        .expect("starts");
    let digest = |bytes: &[u8]| task_model::Digest::sha256(bytes).to_string();
    let source = |kind, path: String, bytes: &[u8]| Source {
        kind,
        path,
        digest: digest(bytes),
        size: bytes.len() as u64,
        executable: false,
    };
    recorder.sources(&[
        source(SourceKind::Workflow, "main.nf".into(), WORKFLOW.as_bytes()),
        source(SourceKind::Named, text(&named), config.as_bytes()),
    ]);
    let mut upper = task(1, "UPPER", vec![], "upper.txt");
    upper.environment.insert("TOKEN".into(), secret.into());
    upper.script = Some(format!("cd {} && echo $TOKEN", text(&work)));
    recorder.requested(&upper, &[]);
    recorder.started(TaskId(1), 1);
    recorder.finished(&finished(1, "upper.txt", output), 1, false);
    recorder.ended(None);
    recorder.close().expect("closes");
    let database = Database::open(&path).expect("opens again");
    let directory = root.path().join("crate");
    let options = Options {
        workflow,
        secrets: vec![secret.into()],
        folders: vec![folder.clone()],
        ..Options::default()
    };
    export(&database, "secret-run", &directory, &options).expect("exports");
    let server = text(&folder);
    for entry in files_below(&directory) {
        let bytes = std::fs::read(&entry).expect("reads a file of the crate");
        let content = String::from_utf8_lossy(&bytes);
        assert!(
            !content.contains(secret),
            "{} holds the secret",
            entry.display()
        );
        let named: Vec<&str> = (content.lines())
            .filter(|line| line.contains(&server))
            .collect();
        assert!(
            named.is_empty(),
            "{} names the server folder: {named:?}",
            entry.display()
        );
    }
    let metadata = std::fs::read_to_string(directory.join(METADATA)).expect("reads metadata");
    assert!(metadata.contains("[secret]"), "{metadata}");
    assert!(metadata.contains("cd work && echo $TOKEN"), "{metadata}");
    assert!(directory.join("main.nf").is_file());
}

/// A main workflow file that holds a secret value of the run is not exported at all.
#[test]
fn refuses_secret_workflow() {
    let root = tempfile::tempdir().expect("a directory");
    let folder = root.path().join("server/run");
    std::fs::create_dir_all(&folder).expect("makes the run folder");
    let secret = "sentinel-secret-value";
    let workflow = folder.join("main.nf");
    let text = format!("params.token = '{secret}'\n{WORKFLOW}");
    std::fs::write(&workflow, &text).expect("writes the workflow");
    let path = |path: &Path| path.to_string_lossy().into_owned();
    let start = RunStart {
        id: "secret-main".into(),
        resumes: None,
        workflow: path(&workflow),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into(), path(&workflow)],
        parameters: vec![],
        executor: "local".into(),
        simulated: false,
        store: None,
        program: Some("uwe 0.1.0".into()),
        launch: Some(path(&folder)),
        workflow_folder: Some(path(&folder)),
        repeats: None,
    };
    let database_path = root.path().join("runs.sqlite");
    let mut recorder = (Database::open(&database_path).expect("opens"))
        .record(&start)
        .expect("starts");
    recorder.sources(&[Source {
        kind: SourceKind::Workflow,
        path: "main.nf".into(),
        digest: task_model::Digest::sha256(text.as_bytes()).to_string(),
        size: text.len() as u64,
        executable: false,
    }]);
    recorder.ended(None);
    recorder.close().expect("closes");
    let database = Database::open(&database_path).expect("opens again");
    let directory = root.path().join("crate");
    let options = Options {
        workflow: workflow.clone(),
        secrets: vec![secret.into()],
        ..Options::default()
    };
    let error = export(&database, "secret-main", &directory, &options).expect_err("refuses");
    let message = error.to_string();
    assert!(message.contains(&path(&workflow)), "{message}");
    assert!(!message.contains(secret), "{message}");
    // The check comes before the crate folder is made, so nothing holds the secret.
    assert!(!directory.exists());
}

#[test]
fn describes_data_files() {
    let root = tempfile::tempdir().expect("a directory");
    let folder = root.path().join("server/run");
    let work = folder.join("work");
    std::fs::create_dir_all(&work).expect("makes the work folder");
    let workflow = folder.join("main.nf");
    std::fs::write(&workflow, WORKFLOW).expect("writes the workflow");
    // A large input whose digest the database lacks, and an output whose digest it holds.
    let large: Vec<u8> = (0..8u32 << 20).map(|at| (at % 251) as u8).collect();
    let reads = folder.join("reads.fq");
    std::fs::write(&reads, &large).expect("writes the input");
    let upper = work.join("upper.txt");
    std::fs::write(&upper, "HELLO\n").expect("writes an output");
    let named = folder.join("extra.config");
    std::fs::write(&named, CONFIG).expect("writes the config");
    let secret = "sentinel-secret-value";
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let start = RunStart {
        id: "large-run".into(),
        resumes: None,
        workflow: text(&workflow),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into(), text(&workflow), "-c".into(), text(&named)],
        parameters: vec![],
        executor: "local".into(),
        simulated: false,
        store: None,
        program: Some("uwe 0.1.0".into()),
        launch: Some(text(&folder)),
        workflow_folder: Some(text(&folder)),
        repeats: None,
    };
    let path = root.path().join("runs.sqlite");
    let mut recorder = (Database::open(&path).expect("opens"))
        .record(&start)
        .expect("starts");
    let digest = |bytes: &[u8]| task_model::Digest::sha256(bytes);
    recorder.sources(&[Source {
        kind: SourceKind::Named,
        path: text(&named),
        digest: digest(CONFIG.as_bytes()).to_string(),
        size: CONFIG.len() as u64,
        executable: false,
    }]);
    let mut first = task(1, "UPPER", vec![input("reads.fq", &reads)], "upper.txt");
    first.environment.insert("TOKEN".into(), secret.into());
    recorder.requested(&first, &[]);
    recorder.started(TaskId(1), 1);
    let mut outcome = finished(1, "upper.txt", upper);
    outcome.files[0].digest = Some(digest(b"HELLO\n").to_string());
    outcome.files[0].size = Some(6);
    recorder.finished(&outcome, 1, false);
    recorder.ended(None);
    recorder.close().expect("closes");
    let database = Database::open(&path).expect("opens again");
    let directory = root.path().join("crate");
    let options = Options {
        workflow,
        secrets: vec![secret.into()],
        folders: vec![folder.clone()],
        describe_data: true,
        ..Options::default()
    };
    let summary = export(&database, "large-run", &directory, &options).expect("exports");
    assert_eq!((summary.files, summary.bytes), (0, 0));
    assert!(!directory.join("data").exists(), "the crate copied data");
    let used: u64 = (files_below(&directory).iter())
        .map(|file| std::fs::metadata(file).expect("reads a size").len())
        .sum();
    assert!(used < 1 << 20, "the crate uses {used} bytes");
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(directory.join(METADATA)).expect("reads metadata"))
            .expect("parses metadata");
    let reads_id = format!("#data/{}", digest(&large).hex());
    let described = entity(&metadata, &reads_id);
    assert_eq!(described["@type"], "File");
    assert_eq!(described["sha256"], digest(&large).hex());
    assert_eq!(described["contentSize"], large.len().to_string());
    assert_eq!(described["contentUrl"], "reads.fq");
    let upper_id = format!("#data/{}", digest(b"HELLO\n").hex());
    let output = entity(&metadata, &upper_id);
    assert_eq!(
        (&output["sha256"], &output["contentSize"]),
        (&Value::from(digest(b"HELLO\n").hex()), &Value::from("6"))
    );
    assert_eq!(output["contentUrl"], "work/upper.txt");
    let config_id = format!("#data/{}", digest(CONFIG.as_bytes()).hex());
    assert_eq!(entity(&metadata, &config_id)["contentUrl"], "extra.config");
    let action = entity(&metadata, "#task/1");
    assert_eq!(action["object"]["@id"], reads_id.as_str());
    assert_eq!(action["result"]["@id"], upper_id.as_str());
    let objects = entity(&metadata, "#run/large-run")["object"].clone();
    assert!(
        (objects.as_array().expect("objects").iter()).any(|object| object["@id"] == config_id),
        "{objects}"
    );
    let parts = entity(&metadata, "./")["hasPart"].to_string();
    assert!(!parts.contains("#data/"), "{parts}");
    let server = text(&folder);
    for entry in files_below(&directory) {
        let content = std::fs::read_to_string(&entry).expect("reads a file of the crate");
        assert!(!content.contains(secret), "{}", entry.display());
        assert!(!content.contains(&server), "{}", entry.display());
    }
}

/// The files below a folder.
fn files_below(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder).expect("lists the folder") {
        let path = entry.expect("an entry").path();
        match path.is_dir() {
            true => files.extend(files_below(&path)),
            false => files.push(path),
        }
    }
    files
}
