//! Exports crates that name files by the persistent addresses of a host and record the wir
//! document of the run with its version and sources.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_crate::{METADATA, Options, UriResolver, export};
use run_database::{Database, RunStart, Source, SourceKind};
use serde_json::Value;
use task_model::Digest;
use workflow_graph::{Input, Language, Outcome, OutputFile, Task, TaskId};

const WORKFLOW: &str =
    "process UPPER {\n    script:\n    \"tr a-z A-Z < reads.fq > upper.txt\"\n}\n";

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Records a run of one task that reads `store/reads.fq` and writes `work/upper.txt`, with
/// the wir document `document` when given.
fn record(root: &Path, document: Option<(&Path, &str)>) -> Database {
    let workflow = root.join("main.nf");
    std::fs::write(&workflow, WORKFLOW).unwrap();
    std::fs::create_dir_all(root.join("store")).unwrap();
    std::fs::create_dir_all(root.join("work")).unwrap();
    let (reads, upper) = (root.join("store/reads.fq"), root.join("work/upper.txt"));
    std::fs::write(&reads, "acgt\n").unwrap();
    std::fs::write(&upper, "ACGT\n").unwrap();
    let start = RunStart {
        id: "kept-run".into(),
        resumes: None,
        workflow: text(&workflow),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into()],
        parameters: Vec::new(),
        executor: "local".into(),
        simulated: false,
        store: None,
        program: None,
        launch: None,
        workflow_folder: Some(text(root)),
        repeats: None,
    };
    let path = root.join("runs.sqlite");
    let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
    if let Some((document, digest)) = document {
        recorder.document(&text(document), digest);
    }
    let task = Task {
        id: TaskId(1),
        name: "UPPER (1)".into(),
        step: "UPPER".into(),
        command: vec!["true".into()],
        inputs: vec![Input {
            path: "reads.fq".into(),
            source: text(&reads),
            ..Input::default()
        }],
        outputs: vec!["upper.txt".into()],
        ..Task::default()
    };
    recorder.requested(&task, &[]);
    recorder.started(TaskId(1), 1);
    let outcome = Outcome {
        id: TaskId(1),
        files: vec![OutputFile {
            path: "upper.txt".into(),
            location: text(&upper),
            digest: Some(Digest::sha256(b"ACGT\n").to_string()),
            size: Some(5),
            ..OutputFile::default()
        }],
        inputs: [("reads.fq".into(), Digest::sha256(b"acgt\n").to_string())].into(),
        ..Outcome::default()
    };
    recorder.finished(&outcome, 1, false);
    recorder.ended(None);
    recorder.close().unwrap();
    Database::open(&path).unwrap()
}

fn exported(root: &Path, database: &Database, options: &Options) -> Value {
    let directory = root.join("crate");
    export(database, "kept-run", &directory, options).unwrap();
    serde_json::from_slice(&std::fs::read(directory.join(METADATA)).unwrap()).unwrap()
}

fn entity<'a>(metadata: &'a Value, id: &str) -> &'a Value {
    (metadata["@graph"].as_array().unwrap().iter())
        .find(|entity| entity["@id"] == id)
        .unwrap_or_else(|| panic!("no entity {id} in {metadata}"))
}

#[test]
fn maps_persistent_uris() {
    let root = tempfile::tempdir().unwrap();
    let database = record(root.path(), None);
    // The host keeps the input, so the crate describes it without the local file.
    std::fs::remove_file(root.path().join("store/reads.fq")).unwrap();
    let upper = text(&root.path().join("work/upper.txt"));
    let options = Options {
        workflow: root.path().join("main.nf"),
        describe_data: true,
        uri_map: [(text(&root.path().join("store/")), "aruna://object/".into())].into(),
        uri_resolver: Some(UriResolver::new(move |location| {
            (location == upper).then(|| "https://data.example.org/runs/upper".into())
        })),
        ..Options::default()
    };
    let metadata = exported(root.path(), &database, &options);
    let reads = entity(&metadata, "aruna://object/reads.fq");
    assert_eq!(reads["contentUrl"], "aruna://object/reads.fq");
    assert_eq!(reads["sha256"], Digest::sha256(b"acgt\n").hex());
    assert_eq!(reads["contentSize"], "5");
    let output = entity(&metadata, "https://data.example.org/runs/upper");
    assert_eq!(output["contentUrl"], "https://data.example.org/runs/upper");
    assert_eq!(output["sha256"], Digest::sha256(b"ACGT\n").hex());
    let action = entity(&metadata, "#task/1");
    assert_eq!(action["object"]["@id"], "aruna://object/reads.fq");
    assert_eq!(
        action["result"]["@id"],
        "https://data.example.org/runs/upper"
    );
    let local = text(root.path());
    for described in [reads, output] {
        assert!(!described.to_string().contains(&local), "{described}");
    }
}

#[test]
fn records_wir_provenance() {
    let root = tempfile::tempdir().unwrap();
    let source = Digest::sha256(b"cwlVersion: v1.2\n").to_string();
    let document = serde_json::json!({
        "wir_version": "0.1",
        "sources": [{"id": "main", "language": "cwl", "location": "main.cwl",
            "version": "v1.2", "digest": source}],
        "graphs": [],
        "entry": "main",
    });
    let bytes = serde_json::to_vec(&document).unwrap();
    let path = root.path().join("main.wir.json");
    std::fs::write(&path, &bytes).unwrap();
    let digest = Digest::sha256(&bytes);
    let database = record(root.path(), Some((&path, &digest.to_string())));
    let options = Options {
        workflow: root.path().join("main.nf"),
        ..Options::default()
    };
    let metadata = exported(root.path(), &database, &options);
    let wir = entity(&metadata, "#wir-document");
    assert_eq!(wir["version"], "0.1");
    assert_eq!(wir["sha256"], digest.hex());
    assert_eq!(wir["isBasedOn"][0]["@id"], "#wir-source/main");
    let declared = entity(&metadata, "#wir-source/main");
    assert_eq!(declared["codeRepository"], "main.cwl");
    assert_eq!(declared["programmingLanguage"], "cwl");
    assert_eq!(declared["version"], "v1.2");
    assert_eq!(declared["sha256"], source.strip_prefix("sha256:").unwrap());
    let mentions = entity(&metadata, "./")["mentions"].to_string();
    assert!(mentions.contains("#wir-document"), "{mentions}");
    // A document that changed since the run is not described as the one it ran.
    std::fs::write(&path, b"{\"wir_version\": \"0.2\"}").unwrap();
    std::fs::remove_dir_all(root.path().join("crate")).unwrap();
    let changed = exported(root.path(), &database, &options);
    assert!(!changed.to_string().contains("#wir-document"));
}

/// A folder keeps its trailing slash under a persistent address, also when the database has
/// its digest and size, and a file that the command line named takes the address of the host.
#[test]
fn keeps_folder_ids() {
    let root = tempfile::tempdir().unwrap();
    let workflow = root.path().join("main.nf");
    std::fs::write(&workflow, WORKFLOW).unwrap();
    let work = root.path().join("work");
    for name in ["kept", "found"] {
        std::fs::create_dir_all(work.join(name)).unwrap();
        std::fs::write(work.join(name).join("part.txt"), name).unwrap();
    }
    let config = root.path().join("params.json");
    std::fs::write(&config, "{}\n").unwrap();
    let start = RunStart {
        id: "kept-run".into(),
        resumes: None,
        workflow: text(&workflow),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into()],
        parameters: Vec::new(),
        executor: "local".into(),
        simulated: false,
        store: None,
        program: None,
        launch: None,
        workflow_folder: Some(text(root.path())),
        repeats: None,
    };
    let path = root.path().join("runs.sqlite");
    let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
    recorder.sources(&[Source {
        kind: SourceKind::Named,
        path: text(&config),
        digest: Digest::sha256(b"{}\n").to_string(),
        size: 3,
        executable: false,
    }]);
    let task = Task {
        id: TaskId(1),
        name: "MAKE (1)".into(),
        step: "MAKE".into(),
        command: vec!["true".into()],
        outputs: vec!["kept".into(), "found".into()],
        ..Task::default()
    };
    recorder.requested(&task, &[]);
    recorder.started(TaskId(1), 1);
    let folder = |name: &str, size| OutputFile {
        path: name.into(),
        location: text(&work.join(name)),
        directory: true,
        digest: Some(Digest::sha256(name.as_bytes()).to_string()),
        size,
    };
    let outcome = Outcome {
        id: TaskId(1),
        files: vec![folder("kept", Some(4)), folder("found", None)],
        ..Outcome::default()
    };
    recorder.finished(&outcome, 1, false);
    recorder.ended(None);
    recorder.close().unwrap();
    let database = Database::open(&path).unwrap();
    let named = text(&config);
    let options = Options {
        workflow,
        describe_data: true,
        uri_map: [(text(&root.path().join("work/")), "aruna://object/".into())].into(),
        uri_resolver: Some(UriResolver::new(move |location| {
            (location == named).then(|| "https://data.example.org/params".into())
        })),
        ..Options::default()
    };
    let metadata = exported(root.path(), &database, &options);
    for name in ["kept", "found"] {
        let shown = entity(&metadata, &format!("aruna://object/{name}/"));
        assert_eq!(shown["url"], format!("aruna://object/{name}"));
    }
    let params = entity(&metadata, "https://data.example.org/params");
    assert_eq!(params["contentUrl"], "https://data.example.org/params");
}

#[test]
fn debug_hides_secrets() {
    let options = Options {
        secrets: vec!["sentinel-secret-value".into()],
        uri_map: [("/data/".into(), "https://reader:map-password@host/".into())].into(),
        ..Options::default()
    };
    let shown = format!("{options:?}");
    assert!(shown.contains("1 redacted"), "{shown}");
    assert!(
        !shown.contains("sentinel-secret-value") && !shown.contains("map-password"),
        "{shown}"
    );
}
