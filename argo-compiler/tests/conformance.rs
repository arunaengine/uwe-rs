//! Compiles the Argo test workflows into wir, runs each natively and compiled on the
//! simulator, and compares the outcomes, tasks, cache keys and outputs with the recorded ones.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use argo_runtime::library::collect_files;
use argo_runtime::{ArgoDriver, Kind, Library, Options, load_file, repository_of};
use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, Task, execute};

/// How many workflows compile fully, stay one native op or do not compile; how many compiled
/// runs end as the native run does (both succeed with as many tasks, or both fail); how many of
/// those that both succeed ask for equal tasks with equal cache keys; and how many give equal
/// outputs, with Files and Directories compared by class and basename.
const RECORDED: Counts = Counts {
    full: 545,
    native: 277,
    failed: 138,
    same: 301,
    keys: 261,
    equal: 253,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    full: usize,
    native: usize,
    failed: usize,
    same: usize,
    keys: usize,
    equal: usize,
}

/// The tasks of a run by node name, each with its task cache key.
type Keyed = BTreeMap<String, Vec<(Json, String)>>;

/// A finished run: its tasks, or its error.
type Ran = Result<Vec<Task>, String>;

fn native(file: &Path, root: &Path) -> (Ran, Option<Json>) {
    let documents = load_file(file).unwrap_or_default();
    let Some(document) = (documents.iter())
        .find(|document| document.kind == Kind::Workflow && document.runnable())
        .cloned()
    else {
        return (Err("no workflow".into()), None);
    };
    let library = Library::new();
    library.add_documents(0, documents);
    library.add_directory(2, repository_of(file));
    let options = Options {
        scratch: root.join("scratch"),
        assume_inputs: true,
        ..Options::default()
    };
    let mut driver = ArgoDriver::new(document, Rc::new(library), options);
    let ran = run(&mut driver, root);
    (ran, driver.exported())
}

fn compiled(document: &Path, root: &Path) -> (Ran, Option<Json>) {
    let launch = Launch {
        target: document.to_owned(),
        scratch: root.join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = match WirDriver::new(&launch, settings) {
        Ok(driver) => driver,
        Err(error) => return (Err(error.message), None),
    };
    let ran = run(&mut driver, root);
    let outputs = driver.outputs();
    (ran, outputs)
}

fn run(driver: &mut dyn Driver, root: &Path) -> Ran {
    let folder = root.join("simulator");
    let _ = std::fs::remove_dir_all(&folder);
    let mut simulator = Simulator::new(&folder);
    let execution = execute("conformance", driver, &mut simulator);
    let graph = execution.into_result().map_err(|error| error.to_string())?;
    Ok(graph.nodes.into_iter().map(|node| node.task).collect())
}

/// The tasks of a run as the comparison sees them, by node name: without ids and the tasks
/// they wait for, input sources by file name, and with their task cache key.
fn keyed(tasks: &[Task], compiled: bool) -> Keyed {
    let mut found = Keyed::new();
    for task in tasks {
        let mut task = task.clone();
        task.id = workflow_graph::TaskId(0);
        task.after.clear();
        // A wir run shows the op path before the display name, which no cache key holds.
        if let Some((_, name)) = task.name.split_once(": ").filter(|_| compiled) {
            task.name = name.to_owned();
        }
        for input in &mut task.inputs {
            let name = Path::new(&input.source).file_name().unwrap_or_default();
            input.source = name.to_string_lossy().into_owned();
        }
        let request = task_runner::request(&task, task.image.clone(), Vec::new(), None);
        let key = request
            .spec
            .cache_key()
            .map(|key| key.to_string())
            .unwrap_or_default();
        let value = serde_json::to_value(&task).unwrap_or_default();
        found
            .entry(task.name.clone())
            .or_default()
            .push((value, key));
    }
    found
}

/// The first task that differs between two runs, and the fields that differ.
fn difference(left: &Keyed, right: &Keyed) -> String {
    let names = left.keys().chain(right.keys());
    for name in names {
        let (Some(one), Some(other)) = (left.get(name), right.get(name)) else {
            return format!("task {name} is in one run only");
        };
        let (Some((one, _)), Some((other, _))) = (one.first(), other.first()) else {
            continue;
        };
        let fields: Vec<&String> = (one.as_object().into_iter().flatten())
            .filter(|(key, value)| other.get(key.as_str()) != Some(value))
            .map(|(key, _)| key)
            .collect();
        if !fields.is_empty() {
            return format!("task {name} differs in {fields:?}");
        }
    }
    "the cache keys differ".into()
}

/// Outputs with each File and Directory as its class and basename, as the folders differ.
fn shown(value: Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.into_iter().map(shown).collect()),
        Json::Object(object) => match object.get("class").and_then(Json::as_str) {
            Some(class @ ("File" | "Directory")) => {
                json!({"class": class, "basename": object.get("basename")})
            }
            _ => Json::Object(object.into_iter().map(|(k, v)| (k, shown(v))).collect()),
        },
        other => other,
    }
}

fn outcome(file: &Path, counts: &mut Counts) -> String {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let compiled_document = match argo_compiler::compile(file, &[], root) {
        Ok(compiled) => {
            match compiled.kept.first() {
                None => counts.full += 1,
                Some(_) => counts.native += 1,
            }
            compiled
        }
        Err(error) => {
            counts.failed += 1;
            return format!("not compiled: {}", first(&error));
        }
    };
    let target = root.join("compiled.wir.json");
    let text = serde_json::to_string(&compiled_document.document).expect("serializes");
    std::fs::write(&target, text).expect("writes the compiled document");
    let (one, one_outputs) = native(file, root);
    let (other, other_outputs) = compiled(&target, root);
    let line = match (&one, &other) {
        (Ok(one), Ok(other)) if one.len() == other.len() => {
            counts.same += 1;
            let (left, right) = (keyed(one, false), keyed(other, true));
            let tasks = left == right;
            if !tasks && std::env::var("ARGO_TEST").is_ok() {
                println!("{}", difference(&left, &right));
            }
            counts.keys += usize::from(tasks);
            let outputs = one_outputs.map(shown) == other_outputs.map(shown);
            counts.equal += usize::from(outputs);
            format!("same, tasks equal {tasks}, outputs equal {outputs}")
        }
        (Err(one), Err(_)) => {
            counts.same += 1;
            format!("same, both fail: {}", first(one))
        }
        (Ok(one), Ok(other)) => format!("differs: {} and {} tasks", one.len(), other.len()),
        (Ok(_), Err(error)) => format!("differs: compiled fails: {}", first(error)),
        (Err(error), Ok(_)) => format!("differs: native fails: {}", first(error)),
    };
    match compiled_document.kept.first() {
        Some(kept) => format!("native ({}: {}), {line}", kept.path, kept.reason),
        None => format!("full, {line}"),
    }
}

fn first(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

#[test]
#[ignore = "needs the Argo test workflows from `just argo-test-workflows`"]
fn matches_recorded_workflows() {
    // Recursive workflows nest templates deeply, which needs more stack than a test thread has.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(compare_workflows)
        .expect("a thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

fn compare_workflows() {
    let root =
        PathBuf::from(std::env::var("ARGO_TEST_WORKFLOWS").expect("ARGO_TEST_WORKFLOWS is set"));
    let filter = std::env::var("ARGO_TEST_FILTER").unwrap_or_default();
    let mut files = Vec::new();
    collect_files(&root, &mut files);
    let mut counts = Counts::default();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        let documents = load_file(&file).unwrap_or_default();
        let runnable = (documents.iter())
            .any(|document| document.kind == Kind::Workflow && document.runnable());
        if !runnable || !relative.contains(&filter) {
            continue;
        }
        let line = outcome(&file, &mut counts);
        if std::env::var("ARGO_TEST").is_ok() {
            println!("{relative} {line}");
        }
    }
    println!("{counts:?}");
    if filter.is_empty() {
        assert_eq!(counts, RECORDED);
    }
}
