//! Runs the task ops of `wir/examples/two-tools.json`: their tasks, success, failure with a
//! retry, a missing output and success codes.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{
    Attempt, EdgeKind, Execution, Executor, Launch, Network, Outcome, OutputFile, Running,
    Simulator, Task, execute,
};

/// The example in `root`, changed by `change`, with a text file and its inputs file.
fn example(root: &Path, change: impl FnOnce(&mut Json)) -> Launch {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../wir/examples/two-tools.json");
    let mut document: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    change(&mut document);
    std::fs::write(root.join("two-tools.json"), document.to_string()).unwrap();
    std::fs::write(root.join("text.txt"), "b\nc\na\n").unwrap();
    let format = "http://edamontology.org/format_2330";
    let text = json!({"class": "File", "location": "text.txt", "format": format});
    let inputs = json!({"text": text, "lines": 2});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    Launch {
        target: root.join("two-tools.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    }
}

fn driver(launch: &Launch) -> WirDriver {
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    WirDriver::new(launch, settings).unwrap()
}

/// Ends each attempt with `exit_code`, and, when `files` is set, with one file of the given
/// text for each output of the task.
struct Scripted {
    root: PathBuf,
    exit_code: fn(&Task) -> i32,
    files: bool,
    attempts: Vec<String>,
}

impl Executor for Scripted {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.attempts.push(task.name.clone());
        let work = self.root.join(format!("{}-{}", task.id, attempt.number));
        std::fs::create_dir_all(&work).unwrap();
        let files = (task.outputs.iter())
            .filter(|_| self.files)
            .map(|path| {
                std::fs::write(work.join(path), "a\n").unwrap();
                OutputFile {
                    path: path.clone(),
                    location: work.join(path).to_string_lossy().into_owned(),
                    size: Some(2),
                    ..OutputFile::default()
                }
            })
            .collect();
        let outcome = Outcome {
            id: task.id,
            exit_code: (self.exit_code)(task),
            files,
            ..Outcome::default()
        };
        Running::ended(Ok(outcome))
    }
}

fn scripted(root: &Path, exit_code: fn(&Task) -> i32, files: bool) -> Scripted {
    Scripted {
        root: root.join("work"),
        exit_code,
        files,
        attempts: Vec::new(),
    }
}

#[test]
fn builds_task_commands() {
    let folder = tempfile::tempdir().unwrap();
    let launch = example(folder.path(), |_| {});
    let mut driver = driver(&Launch {
        simulated: true,
        ..launch
    });
    let mut simulator = Simulator::new(folder.path().join("simulator"));
    let graph = execute("two-tools", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let [sort, head] = &graph.nodes[..] else {
        panic!("two tasks: {graph:?}");
    };
    let (sort, head) = (&sort.task, &head.task);
    assert_eq!((sort.step.as_str(), head.step.as_str()), ("sort", "head"));
    assert_eq!(
        sort.image.as_deref(),
        Some("docker.io/library/debian:12-slim")
    );
    let script = "exec > 'sorted.stdout'\n'sort' 'inputs/text/text.txt'";
    assert_eq!(sort.command, ["/bin/sh", "-c", script]);
    assert_eq!(sort.inputs[0].path, "inputs/text/text.txt");
    assert_eq!(sort.outputs, ["sorted.stdout"]);
    assert_eq!(sort.resources.memory_bytes, Some(268_435_456));
    assert!(head.command[2].ends_with("'head' '-n' '2' 'inputs/sorted/sorted.stdout'"));
    let edge = &graph.edges[0];
    let file = EdgeKind::File {
        path: "inputs/sorted/sorted.stdout".into(),
    };
    assert_eq!((edge.from, edge.to, &edge.kind), (sort.id, head.id, &file));
}

#[test]
fn denies_task_network() {
    let folder = tempfile::tempdir().unwrap();
    let launch = example(folder.path(), |document| {
        document["wir_version"] = json!(wir::VERSION);
        document["graphs"][0]["ops"][1]["kind"]["task"]["network"] = json!(true);
    });
    let mut driver = driver(&Launch {
        simulated: true,
        ..launch
    });
    let mut simulator = Simulator::new(folder.path().join("simulator"));
    let graph = execute("two-tools", &mut driver, &mut simulator)
        .into_result()
        .unwrap();
    let networks: Vec<Network> = graph.nodes.iter().map(|node| node.task.network).collect();
    assert_eq!(
        networks,
        [Network::Denied, Network::Allowed],
        "only head opts in"
    );
}

#[test]
fn names_stream_files() {
    let folder = tempfile::tempdir().unwrap();
    let launch = example(folder.path(), |document| {
        document["wir_version"] = json!(wir::VERSION);
        let sort = &mut document["graphs"][0]["ops"][0]["kind"]["task"];
        sort["stdout"] = json!("sorted.txt");
        sort["stderr"] = json!("log.txt");
    });
    let mut executor = scripted(folder.path(), |_| 0, true);
    let mut driver = driver(&launch);
    let graph = execute("two-tools", &mut driver, &mut executor)
        .into_result()
        .unwrap();
    let sort = &graph.nodes[0].task;
    let script = "exec > 'sorted.txt' 2> 'log.txt'\n'sort' 'inputs/text/text.txt'";
    assert_eq!(sort.command, ["/bin/sh", "-c", script]);
    assert_eq!(sort.outputs, ["sorted.txt", "log.txt"]);
    let head = &graph.nodes[1].task;
    assert!(
        head.command[2].ends_with("'inputs/sorted/sorted.txt'"),
        "the capture reads the named file"
    );
}

/// Runs the example on a scripted executor.
fn run(
    root: &Path,
    executor: &mut Scripted,
    change: impl FnOnce(&mut Json),
) -> (WirDriver, Execution) {
    let mut driver = driver(&example(root, change));
    let execution = execute("two-tools", &mut driver, executor);
    (driver, execution)
}

#[test]
fn binds_task_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let mut executor = scripted(folder.path(), |_| 0, true);
    let (driver, execution) = run(folder.path(), &mut executor, |_| {});
    let graph = execution.into_result().unwrap();
    assert!(graph.nodes.iter().all(|node| node.accepted == Some(true)));
    let top = &workflow_graph::Driver::results(&driver).unwrap()["top"];
    assert_eq!(
        (top["class"].as_str(), top["basename"].as_str()),
        (Some("File"), Some("top.stdout"))
    );
    assert_eq!(top["size"], 2);
    let location = top["location"].as_str().unwrap();
    assert!(
        location.ends_with("/out/top.stdout"),
        "delivered to the output folder: {location}"
    );
}

#[test]
fn retries_failed_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let mut executor = scripted(folder.path(), |_| 1, true);
    let (_, execution) = run(folder.path(), &mut executor, |_| {});
    let error = execution.into_result().unwrap_err().to_string();
    assert_eq!(error, "op sort: the command ended with exit code 1");
    assert_eq!(
        executor.attempts,
        ["sort", "sort"],
        "sort has one retry, head never runs"
    );
}

#[test]
fn fails_missing_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let mut executor = scripted(folder.path(), |_| 0, false);
    let (_, execution) = run(folder.path(), &mut executor, |_| {});
    let error = execution.into_result().unwrap_err().to_string();
    assert_eq!(error, "op sort: output sorted: no file matched");
}

#[test]
fn accepts_success_codes() {
    let folder = tempfile::tempdir().unwrap();
    let mut executor = scripted(
        folder.path(),
        |task| if task.step == "head" { 3 } else { 0 },
        true,
    );
    let codes = |document: &mut Json| {
        document["graphs"][0]["ops"][1]["kind"]["task"]["success_codes"] = json!([0, 3]);
    };
    let (_, execution) = run(folder.path(), &mut executor, codes);
    execution.into_result().unwrap();
    assert_eq!(executor.attempts, ["sort", "head"]);
}

#[test]
fn refuses_output_clashes() {
    let folder = tempfile::tempdir().unwrap();
    let mut executor = scripted(folder.path(), |_| 0, true);
    let (driver, execution) = run(folder.path(), &mut executor, |document| {
        let graph = &mut document["graphs"][0];
        for op in graph["ops"].as_array_mut().unwrap() {
            op["kind"]["task"]["captures"][0]["from"] = json!({"glob": "result.txt"});
        }
        let outputs = graph["outputs"].as_array_mut().unwrap();
        outputs.push(json!({"name": "sorted", "type": {"file": {}}}));
        let edge = json!({"from": {"op": "sort", "port": "sorted"}, "to": {"port": "sorted"}});
        graph["edges"].as_array_mut().unwrap().push(edge);
    });
    let error = execution.into_result().unwrap_err().to_string();
    assert!(error.contains("/out/result.txt"), "{error}");
    assert!(workflow_graph::Driver::results(&driver).is_none());
    assert!(!folder.path().join("out/result.txt").exists());
}

#[test]
fn formats_captured_files() {
    let folder = tempfile::tempdir().unwrap();
    let mut executor = scripted(folder.path(), |_| 0, true);
    let format = "http://edamontology.org/format_2330";
    let (driver, execution) = run(folder.path(), &mut executor, |document| {
        let graph = &mut document["graphs"][0];
        graph["outputs"][0]["type"] = json!({"file": {"format": format}});
        for op in graph["ops"].as_array_mut().unwrap() {
            op["outputs"][0]["type"] = json!({"file": {"format": format}});
        }
    });
    execution.into_result().unwrap();
    let top = &workflow_graph::Driver::results(&driver).unwrap()["top"];
    assert_eq!(top["format"], format, "{top}");
}

#[test]
fn accepts_equal_sources() {
    let folder = tempfile::tempdir().unwrap();
    let launch = example(folder.path(), |document| {
        let graph = &mut document["graphs"][0];
        for name in ["first", "second"] {
            let port = json!({"name": name, "type": {"file": {}}});
            graph["inputs"].as_array_mut().unwrap().push(port.clone());
            graph["outputs"].as_array_mut().unwrap().push(port);
            let edge = json!({"from": {"port": name}, "to": {"port": name}});
            graph["edges"].as_array_mut().unwrap().push(edge);
        }
    });
    let result = folder.path().join("out/result.txt");
    std::fs::create_dir_all(folder.path().join("out")).unwrap();
    std::fs::write(&result, "a\n").unwrap();
    let mut inputs: Json =
        serde_json::from_str(&std::fs::read_to_string(folder.path().join("inputs.json")).unwrap())
            .unwrap();
    let path = result.to_string_lossy().into_owned();
    inputs["first"] = json!({"class": "File", "location": path});
    inputs["second"] = json!({"class": "File", "location": format!("file://{path}")});
    std::fs::write(folder.path().join("inputs.json"), inputs.to_string()).unwrap();
    let mut executor = scripted(folder.path(), |_| 0, true);
    let mut driver = driver(&launch);
    execute("two-tools", &mut driver, &mut executor)
        .into_result()
        .unwrap();
    let results = workflow_graph::Driver::results(&driver).unwrap();
    assert_eq!(results["first"]["location"].as_str(), Some(path.as_str()));
}
