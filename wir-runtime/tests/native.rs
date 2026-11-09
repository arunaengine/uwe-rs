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

/// The results without where they are.
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
    let native = expected["copies"][0]["location"].as_str().unwrap();
    assert_eq!(
        location,
        native.trim_start_matches("file://"),
        "the place of the native run"
    );
    assert!(location.ends_with("/out/a.txt"), "{location}");
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

/// A block that reads the files of another block starts only after the engine delivered
/// them: the call that ends the first block starts nothing, and the next wake does.
#[test]
fn waits_for_deliveries() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let data = root.join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("a.txt"), "one\n").unwrap();
    std::fs::write(
        root.join("job.json"),
        json!({"folder": {"class": "Directory", "location": "data"}}).to_string(),
    )
    .unwrap();
    let joined = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n\
                  inputs:\n  texts:\n    type: File[]\n    inputBinding: {position: 1}\n\
                  outputs:\n  joined: {type: stdout}\nstdout: joined.txt\n";
    std::fs::write(root.join("cat.cwl"), joined).unwrap();
    let files = json!({"array": {"items": {"file": {}}}});
    let document = json!({
        "wir_version": "0.1",
        "sources": [
            {"id": "copy", "language": "cwl", "location": demos().join("directory-copy.cwl")},
            {"id": "cat", "language": "cwl", "location": root.join("cat.cwl")}
        ],
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "folder", "type": "directory"}],
            "ops": [
                {
                    "path": "first", "kind": {"native": {"source": "copy", "language": "cwl"}},
                    "inputs": [{"name": "folder", "type": "directory"}],
                    "outputs": [{"name": "copies", "type": files}]
                },
                {
                    "path": "second", "kind": {"native": {"source": "cat", "language": "cwl"}},
                    "inputs": [{"name": "texts", "type": files}]
                }
            ],
            "edges": [
                {"from": {"port": "folder"}, "to": {"op": "first", "port": "folder"}},
                {"from": {"op": "first", "port": "copies"}, "to": {"op": "second", "port": "texts"}}
            ]
        }],
        "entry": "main"
    });
    let target = root.join("run.wir.json");
    std::fs::write(&target, document.to_string()).unwrap();
    let options = cwl_runtime::Options {
        simulated: false,
        print: false,
        ..cwl_runtime::Options::default()
    };
    let settings = Settings {
        cwl: options,
        ..settings()
    };
    let launch = launch(root, target, root.join("job.json"), false);
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    driver.use_files(std::sync::Arc::new(workflow_graph::LocalFiles));
    let first = driver.start().unwrap();
    assert_eq!(first.len(), 1, "{first:?}");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    let outcome = Outcome {
        id: first[0].id,
        files: vec![OutputFile {
            path: "a.txt".into(),
            location: work.join("a.txt").to_string_lossy().into_owned(),
            ..OutputFile::default()
        }],
        ..Outcome::default()
    };
    let next = driver.complete(outcome).unwrap();
    assert!(next.is_empty(), "the second block waits: {next:?}");
    assert!(
        driver
            .wake_at()
            .is_some_and(|time| time <= std::time::Instant::now())
    );
    let deliveries = driver.deliveries();
    assert!(!deliveries.is_empty());
    for delivery in &deliveries {
        workflow_graph::Files::deliver(&workflow_graph::LocalFiles, delivery).unwrap();
        driver.delivered(delivery).unwrap();
    }
    let second = driver.wake().unwrap();
    assert_eq!(second.len(), 1, "{second:?}");
    assert!(second[0].name.starts_with("second: "), "{}", second[0].name);
    assert_eq!(driver.wake_at(), None);
}

/// A source at a repository address with a `git:` digest runs from the checkout of that
/// commit, which the run fetches; a run that cannot fetch says so.
#[test]
fn fetches_pinned_repositories() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    std::fs::write(root.join("job.json"), "{}").unwrap();
    let commit = "32893afef8076a03a2767a020b3f0cab2e0b40b2";
    let document = json!({
        "wir_version": "0.1",
        "sources": [{
            "id": "hello", "language": "cwl", "location": "https://example.org/demo/hello",
            "digest": format!("git:{commit}")
        }],
        "graphs": [{
            "name": "main",
            "ops": [{"path": "block", "kind": {"native": {"source": "hello", "language": "cwl"}}}]
        }],
        "entry": "main"
    });
    let target = root.join("run.wir.json");
    std::fs::write(&target, document.to_string()).unwrap();
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = asked.clone();
    let fetching = Settings {
        repositories: wir_runtime::Repositories::new(move |location: &str, commit: &str| {
            seen.lock()
                .unwrap()
                .push((location.to_owned(), commit.to_owned()));
            Ok(demos().join("hello_world.cwl"))
        }),
        ..settings()
    };
    let launch = launch(root, target.clone(), root.join("job.json"), true);
    let mut driver = WirDriver::new(&launch, fetching).unwrap();
    let mut simulator = Simulator::new(root.join("simulator"));
    let ran = execute("hello", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    assert_eq!(ran.nodes.len(), 1);
    let expected = (
        "https://example.org/demo/hello".to_owned(),
        commit.to_owned(),
    );
    assert_eq!(*asked.lock().unwrap(), [expected]);
    let mut driver = WirDriver::new(&launch, settings()).unwrap();
    let mut simulator = Simulator::new(root.join("other"));
    let error = execute("hello", &mut driver, &mut simulator)
        .into_result()
        .unwrap_err();
    assert!(
        error.to_string().contains("cannot fetch repositories"),
        "{error}"
    );
}

/// A graph output named like a block goes to the output folder, where the block, as in a
/// native run, delivered none of its own files.
#[test]
fn keeps_block_files() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    std::fs::create_dir(root.join("data")).unwrap();
    std::fs::write(root.join("data/a.txt"), "one\n").unwrap();
    std::fs::create_dir(root.join("other")).unwrap();
    std::fs::write(root.join("other/block"), "two\n").unwrap();
    let job = json!({
        "folder": {"class": "Directory", "location": "data"},
        "extra": {"class": "File", "location": "other/block"}
    });
    std::fs::write(root.join("job.json"), job.to_string()).unwrap();
    let source = demos().join("directory-copy.cwl");
    let extra = json!({"name": "extra", "type": {"file": {}}});
    let document = json!({
        "wir_version": "0.1",
        "sources": [{"id": "block", "language": "cwl", "location": source}],
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "folder", "type": "directory"}, extra],
            "outputs": [extra],
            "ops": [{
                "path": "block", "kind": {"native": {"source": "block", "language": "cwl"}},
                "inputs": [{"name": "folder", "type": "directory"}],
                "outputs": [{"name": "copies", "type": {"array": {"items": {"file": {}}}}}]
            }],
            "edges": [
                {"from": {"port": "folder"}, "to": {"op": "block", "port": "folder"}},
                {"from": {"port": "extra"}, "to": {"port": "extra"}}
            ]
        }],
        "entry": "main"
    });
    let target = root.join("run.wir.json");
    std::fs::write(&target, document.to_string()).unwrap();
    let settings = Settings {
        cwl: cwl_runtime::Options {
            simulated: false,
            print: false,
            ..cwl_runtime::Options::default()
        },
        ..settings()
    };
    let launch = launch(root, target, root.join("job.json"), false);
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut runner = Runner {
        root: root.join("work"),
    };
    execute("copy", &mut driver, &mut runner)
        .into_result()
        .unwrap();
    let delivered = std::fs::read_to_string(root.join("out/block")).unwrap();
    assert_eq!(
        delivered, "two\n",
        "the graph output, not a folder of the block"
    );
    assert!(
        !root.join("out/a.txt").exists(),
        "the copies are no output of the run"
    );
}

/// Ends each task like a run of it: copies the files of its folder inputs and writes the files
/// that it names.
struct Runner {
    root: PathBuf,
}

impl Executor for Runner {
    fn run(&mut self, attempt: Attempt) -> Running {
        let work = self.root.join(attempt.task.id.to_string());
        std::fs::create_dir_all(&work).unwrap();
        let mut names = Vec::new();
        for input in attempt.task.inputs.iter().filter(|input| input.directory) {
            for entry in std::fs::read_dir(&input.source).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                std::fs::copy(entry.path(), work.join(&name)).unwrap();
                names.push(name);
            }
        }
        let named = attempt.task.outputs.iter();
        for name in named.filter(|name| !name.contains('*') && *name != "cwl.output.json") {
            std::fs::write(work.join(name), "joined\n").unwrap();
            names.push(name.clone());
        }
        names.sort();
        let files = (names.into_iter())
            .map(|name| OutputFile {
                location: work.join(&name).to_string_lossy().into_owned(),
                path: name,
                ..OutputFile::default()
            })
            .collect();
        let outcome = Outcome {
            id: attempt.task.id,
            files,
            ..Outcome::default()
        };
        Running::ended(Ok(outcome))
    }

    fn files(&self) -> Option<std::sync::Arc<dyn workflow_graph::Files>> {
        Some(std::sync::Arc::new(workflow_graph::LocalFiles))
    }
}

/// The files below a folder, by their paths in it.
fn files_below(folder: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.path().is_dir() {
            true => found.extend(
                files_below(&entry.path())
                    .iter()
                    .map(|item| format!("{name}/{item}")),
            ),
            false => found.push(name),
        }
    }
    found.sort();
    found
}

/// The targets of the deliveries of a run, which the run database records.
#[derive(Default)]
struct Delivered(Vec<String>);

impl workflow_graph::Observer for Delivered {
    fn event(&mut self, event: workflow_graph::Event<'_>) {
        if let workflow_graph::Event::Delivered { delivery } = event {
            self.0.push(delivery.target.clone());
        }
    }
}

/// A run of two CWL blocks delivers only its output, at the top of the output folder, as a
/// native run does; the file that the first block gives the second is no delivery of it.
#[test]
fn delivers_run_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    std::fs::create_dir(root.join("data")).unwrap();
    std::fs::write(root.join("data/a.txt"), "one\n").unwrap();
    let job = json!({"folder": {"class": "Directory", "location": "data"}});
    std::fs::write(root.join("job.json"), job.to_string()).unwrap();
    let joined = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n\
                  inputs:\n  texts:\n    type: File[]\n    inputBinding: {position: 1}\n\
                  outputs:\n  joined: {type: stdout}\nstdout: joined.txt\n";
    std::fs::write(root.join("cat.cwl"), joined).unwrap();
    let files = json!({"array": {"items": {"file": {}}}});
    let file = json!({"file": {}});
    let document = json!({
        "wir_version": "0.1",
        "sources": [
            {"id": "copy", "language": "cwl", "location": demos().join("directory-copy.cwl")},
            {"id": "cat", "language": "cwl", "location": root.join("cat.cwl")}
        ],
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "folder", "type": "directory"}],
            "outputs": [{"name": "joined", "type": file}],
            "ops": [
                {
                    "path": "first", "kind": {"native": {"source": "copy", "language": "cwl"}},
                    "inputs": [{"name": "folder", "type": "directory"}],
                    "outputs": [{"name": "copies", "type": files}]
                },
                {
                    "path": "second", "kind": {"native": {"source": "cat", "language": "cwl"}},
                    "inputs": [{"name": "texts", "type": files}],
                    "outputs": [{"name": "joined", "type": file}]
                }
            ],
            "edges": [
                {"from": {"port": "folder"}, "to": {"op": "first", "port": "folder"}},
                {"from": {"op": "first", "port": "copies"}, "to": {"op": "second", "port": "texts"}},
                {"from": {"op": "second", "port": "joined"}, "to": {"port": "joined"}}
            ]
        }],
        "entry": "main"
    });
    let target = root.join("run.wir.json");
    std::fs::write(&target, document.to_string()).unwrap();
    let settings = Settings {
        cwl: cwl_runtime::Options {
            simulated: false,
            print: false,
            ..cwl_runtime::Options::default()
        },
        ..settings()
    };
    let launch = launch(root, target, root.join("job.json"), false);
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut runner = Runner {
        root: root.join("work"),
    };
    let mut delivered = Delivered::default();
    let engine = workflow_graph::Engine::new(&mut runner).observer(&mut delivered);
    engine.run("two", &mut driver).into_result().unwrap();
    assert_eq!(
        files_below(&root.join("out")),
        ["joined.txt"],
        "only the run output"
    );
    let joined = root.join("out/joined.txt").to_string_lossy().into_owned();
    assert_eq!(
        delivered.0,
        [joined],
        "the copies of the blocks are no deliveries"
    );
    let location = driver.results().unwrap()["joined"]["location"].clone();
    assert_eq!(
        location,
        root.join("out/joined.txt").to_string_lossy().as_ref()
    );
}

#[test]
fn stops_cancelled_blocks() {
    // Each round of `proc` runs a CWL block; the first fails, which cancels the second.
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let tool = demos().join("hello_world.cwl");
    let message = |flow: &str| json!({"name": "message", "type": "string", "flow": flow});
    let block = json!({"path": "block", "kind": {"native": {"source": "hello", "language": "cwl"}},
        "inputs": [message("value")]});
    let say = json!({"name": "say", "inputs": [message("value")], "ops": [block],
        "edges": [{"from": {"port": "message"}, "to": {"op": "block", "port": "message"}}]});
    let proc = json!({"path": "proc", "kind": {"process": {"body": "say"}}, "inputs": [message("stream")]});
    let main = json!({"name": "main", "inputs": [message("stream")], "ops": [proc],
        "edges": [{"from": {"port": "message"}, "to": {"op": "proc", "port": "message"}}]});
    let document = json!({"wir_version": "0.1",
        "sources": [{"id": "hello", "language": "cwl", "location": tool}],
        "graphs": [main, say], "entry": "main"});
    std::fs::write(root.join("run.wir.json"), document.to_string()).unwrap();
    std::fs::write(
        root.join("job.json"),
        json!({"message": ["a", "b"]}).to_string(),
    )
    .unwrap();
    let launch = Launch {
        keep_going: true,
        ..launch(
            root,
            root.join("run.wir.json"),
            root.join("job.json"),
            false,
        )
    };
    let mut driver = WirDriver::new(&launch, settings()).unwrap();
    let tasks = driver.start().unwrap();
    assert_eq!(tasks.len(), 2, "each round's block asks for its task");
    let failed = Outcome {
        id: tasks[0].id,
        attempt: 1,
        exit_code: 1,
        ..Outcome::default()
    };
    driver.complete(failed).unwrap();
    // The ops after a block that ended start on the next wake.
    assert!(driver.wake_at().is_some());
    driver.wake().unwrap();
    assert_eq!(
        driver.cancellations(),
        [tasks[1].id],
        "the other block stops"
    );
    let cancelled = workflow_graph::ExecuteError::Cancelled;
    assert!(
        driver.handles_error(tasks[1].id, &cancelled),
        "its cancellation is no run error"
    );
    let stopped = Outcome {
        id: tasks[1].id,
        attempt: 1,
        error: Some(workflow_graph::TaskError::Cancelled),
        ..Outcome::default()
    };
    assert!(
        driver.complete(stopped).unwrap().is_empty(),
        "its outcome is dropped"
    );
    let error = driver.finish().unwrap_err().message;
    assert!(error.contains("proc"), "{error}");
}
