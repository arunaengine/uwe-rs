//! Runs branch ops: a skipped round of a map gives null at its place.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Outcome, OutputFile, Running, execute};

fn edge(from: (Option<&str>, &str), to: (Option<&str>, &str)) -> Json {
    json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}})
}

/// A map `each` over `texts` and `gates`, whose round runs the branch `maybe`, which runs the
/// task `cat` when its gate holds.
fn launch(root: &Path, version: &str, gates: &[bool]) -> Launch {
    let file = json!({"file": {}});
    let optional = json!({"optional": file});
    let ports = |text: &Json, gate: &Json| json!([{"name": "text", "type": text}, {"name": "go", "type": gate}]);
    let arrays = ports(
        &json!({"array": {"items": file}}),
        &json!({"array": {"items": "bool"}}),
    );
    let items = ports(&file, &json!("bool"));
    let task = json!({"task": {
        "image": "debian:stable-slim",
        "command": ["cat", {"input": "text"}],
        "captures": [{"output": "out", "from": {"glob": "*.out"}}]
    }});
    let document = json!({
        "wir_version": version,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "texts", "type": arrays[0]["type"]}, {"name": "gates", "type": arrays[1]["type"]}],
            "outputs": [{"name": "outs", "type": {"array": {"items": optional}}}],
            "ops": [{
                "path": "each",
                "kind": {"map": {"body": "round", "over": ["text", "go"]}},
                "inputs": arrays,
                "outputs": [{"name": "out", "type": {"array": {"items": optional}}}]
            }],
            "edges": [
                edge((None, "texts"), (Some("each"), "text")),
                edge((None, "gates"), (Some("each"), "go")),
                edge((Some("each"), "out"), (None, "outs"))
            ]
        }, {
            "name": "round",
            "inputs": items,
            "outputs": [{"name": "out", "type": optional}],
            "ops": [{
                "path": "maybe",
                "kind": {"branch": {"body": "inner", "condition": "go"}},
                "inputs": items,
                "outputs": [{"name": "out", "type": optional}]
            }],
            "edges": [
                edge((None, "text"), (Some("maybe"), "text")),
                edge((None, "go"), (Some("maybe"), "go")),
                edge((Some("maybe"), "out"), (None, "out"))
            ]
        }, {
            "name": "inner",
            "inputs": [{"name": "text", "type": file}],
            "outputs": [{"name": "out", "type": file}],
            "ops": [{
                "path": "cat", "kind": task,
                "inputs": [{"name": "text", "type": file}],
                "outputs": [{"name": "out", "type": file}]
            }],
            "edges": [
                edge((None, "text"), (Some("cat"), "text")),
                edge((Some("cat"), "out"), (None, "out"))
            ]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("branch.wir.json"), document.to_string()).unwrap();
    let texts: Vec<Json> = (0..gates.len())
        .map(|index| {
            std::fs::write(root.join(format!("t{index}.txt")), "text\n").unwrap();
            json!({"class": "File", "location": format!("t{index}.txt")})
        })
        .collect();
    let inputs = json!({"texts": texts, "gates": gates});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    Launch {
        target: root.join("branch.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    }
}

/// Writes `<basename of the last word>.out` for each task, and records the task names and steps.
struct Echoing {
    root: std::path::PathBuf,
    names: Vec<String>,
    steps: Vec<String>,
}

impl Executor for Echoing {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.names.push(task.name.clone());
        self.steps.push(task.step.clone());
        let word = task.command.last().unwrap();
        let path = format!("{}.out", word.rsplit('/').next().unwrap());
        let location = self.root.join(format!("work-{}-{path}", task.id));
        std::fs::write(&location, "").unwrap();
        let files = vec![OutputFile {
            location: location.to_string_lossy().into_owned(),
            path,
            ..OutputFile::default()
        }];
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }
}

#[test]
fn skips_false_rounds() {
    let folder = tempfile::tempdir().unwrap();
    let launch = launch(folder.path(), wir::VERSION, &[true, false, true]);
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut executor = Echoing {
        root: folder.path().to_owned(),
        names: Vec::new(),
        steps: Vec::new(),
    };
    execute("branch", &mut driver, &mut executor)
        .into_result()
        .unwrap();
    assert_eq!(executor.names, ["each/0/maybe/cat", "each/2/maybe/cat"]);
    let outs = &driver.results().unwrap()["outs"];
    let names: Vec<Json> = (outs.as_array().unwrap().iter())
        .map(|item| item.get("basename").cloned().unwrap_or(Json::Null))
        .collect();
    assert_eq!(
        names,
        [json!("t0.txt.out"), Json::Null, json!("t2.txt.out")]
    );
}

#[test]
fn names_by_attributes() {
    let folder = tempfile::tempdir().unwrap();
    let launch = launch(folder.path(), wir::VERSION, &[true, false, true]);
    let mut document: Json =
        serde_json::from_slice(&std::fs::read(&launch.target).unwrap()).unwrap();
    // The map names its rounds by position, and the ops in its bodies add no names.
    document["graphs"][0]["ops"][0]["attributes"] = json!({"name": "each"});
    document["graphs"][1]["ops"][0]["attributes"] = json!({"name": ""});
    document["graphs"][2]["ops"][0]["attributes"] = json!({"name": ""});
    std::fs::write(&launch.target, document.to_string()).unwrap();
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut executor = Echoing {
        root: folder.path().to_owned(),
        names: Vec::new(),
        steps: Vec::new(),
    };
    execute("branch", &mut driver, &mut executor)
        .into_result()
        .unwrap();
    assert_eq!(executor.names, ["each[0]", "each[2]"]);
    assert_eq!(executor.steps, ["each", "each"]);
}
