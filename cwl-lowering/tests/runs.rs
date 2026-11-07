//! Runs CWL fixtures natively and through their lowered wir documents on the simulator, and
//! compares the commands of their tasks and their outputs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options};
use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Driver, Launch, Simulator, execute};

/// The steps and command words of the tasks of a run, and its outputs.
#[derive(Debug, PartialEq)]
struct Ran {
    tasks: Vec<(String, Vec<String>)>,
    outputs: Json,
}

fn run(driver: &mut dyn Driver, root: &Path, name: &str) -> Ran {
    let mut simulator = Simulator::new(root.join(format!("simulator-{name}")));
    let execution = execute(name, driver, &mut simulator);
    let graph = execution.into_result().expect("the run succeeds");
    let mut tasks: Vec<(String, Vec<String>)> = (graph.nodes.iter())
        .map(|node| (step(&node.task.step), words(&node.task)))
        .collect();
    tasks.sort();
    let outputs = shown(driver.results().expect("a real run has results"));
    Ran { tasks, outputs }
}

/// A step without the op path that a map or a branch repeats, like `say` for `say/say`,
/// which docs/wir.md names as a difference of the step names.
fn step(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((head, last)) if head.rsplit('/').next() == Some(last) => head.to_owned(),
        _ => path.to_owned(),
    }
}

/// The words of a command without the shell around it, with paths as their last segment, as
/// the CWL driver and a wir task stage inputs in other folders.
fn words(task: &workflow_graph::Task) -> Vec<String> {
    let text = task
        .script
        .clone()
        .unwrap_or_else(|| task.command.join(" "));
    (text.split_whitespace())
        .map(|word| word.trim_matches('\''))
        .map(|word| word.rsplit('/').next().unwrap_or(word).to_owned())
        .collect()
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

/// Runs a fixture with a job natively and lowered, in a folder of their own.
fn both(name: &str, job: Json) -> (Ran, Ran) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in std::fs::read_dir(&fixtures).expect("lists the fixtures") {
        let path = entry.expect("an entry").path();
        std::fs::copy(&path, root.join(path.file_name().unwrap())).expect("copies");
    }
    std::fs::write(root.join("job.json"), job.to_string()).expect("writes the job");
    let document = cwl_parser::load_file(root.join(name), Default::default()).expect("loads");
    let lowering = cwl_lowering::lower(&document, root).expect("lowers");
    assert!(lowering.full(), "{name}: {:?}", lowering.kept);
    let text = serde_json::to_string(&lowering.document).expect("serializes");
    std::fs::write(root.join("lowered.wir.json"), text).expect("writes");
    let launch = |target: &str, output: &str| Launch {
        target: root.join(target),
        inputs: Some(root.join("job.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join(output)),
        ..Launch::default()
    };
    let options = Options {
        print: false,
        simulated: false,
        ..Options::default()
    };
    let mut native = CwlDriver::new(&launch(name, "native"), options).expect("a CWL driver");
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let target = launch("lowered.wir.json", "lowered");
    let mut lowered = WirDriver::new(&target, settings).expect("a wir driver");
    (
        run(&mut native, root, "native"),
        run(&mut lowered, root, "lowered"),
    )
}

#[test]
fn defaults_match_native() {
    let (native, lowered) = both("defaults.cwl", json!({}));
    assert_eq!(native.tasks.len(), 3, "{native:?}");
    assert_eq!(lowered, native);
}

#[test]
fn values_match_native() {
    let job = json!({"name": "you", "names": ["a", "b"]});
    let (native, lowered) = both("values.cwl", job);
    assert_eq!(native.tasks.len(), 3, "{native:?}");
    assert_eq!(lowered, native);
}

#[test]
fn links_match_native() {
    let job = json!({"name": "you", "names": ["a", "b"]});
    let (native, lowered) = both("links.cwl", job);
    assert_eq!(native.tasks.len(), 5, "{native:?}");
    assert_eq!(lowered, native);
}

#[test]
#[ignore = "needs the CWL JavaScript helper image, which `just test` builds"]
fn expressions_match_native() {
    let (native, lowered) = both("sum.cwl", json!({"a": 1}));
    assert_eq!(native.outputs, json!({"sum": 3, "both": [1, 2]}));
    assert_eq!(lowered, native);
}
