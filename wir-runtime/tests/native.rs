//! Runs demo CWL documents natively and as the native op of a wir document, and compares
//! the tasks, the exported outputs and the failures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver, cwl};
use workflow_graph::{
    Attempt, Driver, Executor, Launch, Outcome, OutputFile, Running, Simulator, Task, TaskId,
    execute,
};

/// The folder of the CWL demos.
fn demos() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../demo/cwl")
}

/// A wir document with one native op `block` that runs `source`, with the same ports
/// on the graph and the op.
fn document(root: &Path, source: &Path, inputs: Json, outputs: Json) -> PathBuf {
    let edges: Vec<Json> = (inputs.as_array().unwrap().iter())
        .map(|port| json!({"from": {"port": port["name"]}, "to": {"op": "block", "port": port["name"]}}))
        .chain(outputs.as_array().unwrap().iter().map(|port| {
            json!({"from": {"op": "block", "port": port["name"]}, "to": {"port": port["name"]}})
        }))
        .collect();
    let document = json!({
        "wir_version": "0.1",
        "sources": [{"id": "block", "language": "cwl", "location": source}],
        "graphs": [{
            "name": "main", "inputs": inputs, "outputs": outputs,
            "ops": [{
                "path": "block", "kind": {"native": {"source": "block", "language": "cwl"}},
                "inputs": inputs, "outputs": outputs
            }],
            "edges": edges
        }],
        "entry": "main"
    });
    let path = root.join("run.wir.json");
    std::fs::write(&path, document.to_string()).unwrap();
    path
}

fn launch(root: &Path, target: PathBuf, inputs: PathBuf, simulated: bool) -> Launch {
    Launch {
        target,
        inputs: Some(inputs),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        simulated,
        ..Launch::default()
    }
}

fn settings() -> Settings {
    Settings {
        print: false,
        ..Settings::default()
    }
}

/// A task without what only names it in its run: its id, its name, and the ids it runs after.
fn described(task: &Task) -> Task {
    Task {
        id: TaskId(0),
        name: String::new(),
        after: Vec::new(),
        ..task.clone()
    }
}

#[test]
fn matches_native_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let workflow = demos().join("scatter-workflow.cwl");
    let native = launch(
        root,
        workflow.clone(),
        demos().join("scatter-job.yml"),
        true,
    );
    let mut driver = cwl_runtime::driver(&native).unwrap();
    let mut simulator = Simulator::new(root.join("native"));
    let alone = execute("scatter", driver.as_mut(), &mut simulator)
        .into_result()
        .unwrap();
    let messages = [
        "Hello world!",
        "Hola mundo!",
        "Bonjour le monde!",
        "Hallo welt!",
    ];
    std::fs::write(
        root.join("inputs.json"),
        json!({"message_array": messages}).to_string(),
    )
    .unwrap();
    let port = json!([{"name": "message_array", "type": {"array": {"items": "string"}}}]);
    let target = document(root, &workflow, port, json!([]));
    let mut driver = WirDriver::new(
        &launch(root, target, root.join("inputs.json"), true),
        settings(),
    )
    .unwrap();
    let mut simulator = Simulator::new(root.join("wir"));
    let through = execute("scatter", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(alone.nodes.len(), 4);
    assert_eq!(alone.nodes.len(), through.nodes.len());
    for (one, other) in alone.nodes.iter().zip(&through.nodes) {
        assert_eq!(described(&one.task), described(&other.task));
        assert_eq!(other.task.name, format!("block: {}", one.task.name));
    }
}

/// Runs each task by copying the text files of its directory inputs into a work folder of
/// its own, as `directory-copy.cwl` does.
struct Copier {
    root: PathBuf,
}

impl Executor for Copier {
    fn run(&mut self, attempt: Attempt) -> Running {
        let work = self.root.join(attempt.task.id.to_string());
        std::fs::create_dir_all(&work).unwrap();
        let mut files = Vec::new();
        for input in attempt.task.inputs.iter().filter(|input| input.directory) {
            for entry in std::fs::read_dir(&input.source).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                std::fs::copy(entry.path(), work.join(&name)).unwrap();
                files.push(OutputFile {
                    location: work.join(&name).to_string_lossy().into_owned(),
                    path: name,
                    ..OutputFile::default()
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let outcome = Outcome {
            id: attempt.task.id,
            files,
            ..Outcome::default()
        };
        Running::ended(Ok(outcome))
    }
}

/// The results without where they are: a run through wir delivers to the op path.
fn placeless(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(placeless).collect()),
        Json::Object(object) => (object.iter())
            .filter(|(key, _)| *key != "location")
            .map(|(key, item)| (key.clone(), placeless(item)))
            .collect(),
        other => other.clone(),
    }
}

#[test]
fn exports_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let data = root.join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("a.txt"), "one\n").unwrap();
    std::fs::write(data.join("b.txt"), "two two\n").unwrap();
    let job = json!({"folder": {"class": "Directory", "location": "data"}});
    std::fs::write(root.join("job.json"), job.to_string()).unwrap();
    let tool = demos().join("directory-copy.cwl");
    let native = launch(root, tool.clone(), root.join("job.json"), false);
    let options = cwl_runtime::Options {
        simulated: false,
        print: false,
        ..cwl_runtime::Options::default()
    };
    let mut alone = cwl_runtime::CwlDriver::new(&native, options.clone()).unwrap();
    let mut copier = Copier {
        root: root.join("native"),
    };
    execute("copy", &mut alone, &mut copier)
        .into_result()
        .unwrap();
    let inputs = json!([{"name": "folder", "type": "directory"}]);
    let outputs = json!([{"name": "copies", "type": {"array": {"items": {"file": {}}}}}]);
    let target = document(root, &tool, inputs, outputs);
    let settings = Settings {
        cwl: options,
        ..settings()
    };
    let mut driver = WirDriver::new(
        &launch(root, target, root.join("job.json"), false),
        settings,
    )
    .unwrap();
    let mut copier = Copier {
        root: root.join("wir"),
    };
    execute("copy", &mut driver, &mut copier)
        .into_result()
        .unwrap();
    let expected = cwl::to_wir(alone.results().unwrap());
    let results = driver.results().unwrap();
    assert_eq!(results["copies"].as_array().unwrap().len(), 2, "{results}");
    assert_eq!(placeless(&results), placeless(&expected));
    let location = results["copies"][0]["location"].as_str().unwrap();
    assert!(location.contains("/out/block/a.txt"), "{location}");
}

#[test]
fn checks_block_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    std::fs::create_dir(root.join("data")).unwrap();
    let job = json!({"folder": {"class": "Directory", "location": "data"}});
    std::fs::write(root.join("job.json"), job.to_string()).unwrap();
    let inputs = json!([{"name": "folder", "type": "directory"}]);
    let outputs = json!([{"name": "copies", "type": "string"}]);
    let target = document(root, &demos().join("directory-copy.cwl"), inputs, outputs);
    let mut driver = WirDriver::new(
        &launch(root, target, root.join("job.json"), true),
        settings(),
    )
    .unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let error = execute("copy", &mut driver, &mut simulator)
        .into_result()
        .unwrap_err();
    assert!(
        error.to_string().contains("output copies does not fit"),
        "{error}"
    );
}

#[test]
fn reports_missing_sources() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    std::fs::write(root.join("job.json"), "{}").unwrap();
    let target = document(root, &root.join("missing.cwl"), json!([]), json!([]));
    let mut driver = WirDriver::new(
        &launch(root, target, root.join("job.json"), true),
        settings(),
    )
    .unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let error = execute("missing", &mut driver, &mut simulator)
        .into_result()
        .unwrap_err();
    assert!(error.to_string().starts_with("op block: "), "{error}");
}
