//! Runs map ops: their rounds in each mode, their outputs in round order, empty inputs and
//! inputs of unequal lengths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};
use wir_runtime::{Settings, WirDriver};
use workflow_graph::{Attempt, Driver, Executor, Launch, Outcome, OutputFile, Running, execute};

/// A document whose map `each` runs the task `cat` once per round of `texts` and `labels`.
fn document(root: &Path, mode: &str, texts: usize, labels: usize) -> Launch {
    let files = json!({"array": {"items": {"file": {}}}});
    let words = json!({"array": {"items": "string"}});
    let depth = if mode == "nested_cross" { 2 } else { 1 };
    let mut outputs = json!({"file": {}});
    for _ in 0..depth {
        outputs = json!({"array": {"items": outputs}});
    }
    let edge = |from: (Option<&str>, &str), to: (Option<&str>, &str)| json!({"from": {"op": from.0, "port": from.1}, "to": {"op": to.0, "port": to.1}});
    let document = json!({
        "wir_version": wir::VERSION,
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "texts", "type": files}, {"name": "labels", "type": words}],
            "outputs": [{"name": "outs", "type": outputs}],
            "ops": [{
                "path": "each",
                "kind": {"map": {"body": "round", "over": ["text", "label"], "mode": mode}},
                "inputs": [{"name": "text", "type": files}, {"name": "label", "type": words}],
                "outputs": [{"name": "out", "type": outputs}]
            }],
            "edges": [
                edge((None, "texts"), (Some("each"), "text")),
                edge((None, "labels"), (Some("each"), "label")),
                edge((Some("each"), "out"), (None, "outs"))
            ]
        }, {
            "name": "round",
            "inputs": [{"name": "text", "type": {"file": {}}}, {"name": "label", "type": "string"}],
            "outputs": [{"name": "out", "type": {"file": {}}}],
            "ops": [{
                "path": "cat",
                "kind": {"task": {
                    "image": "debian:stable-slim",
                    "command": ["cat", {"input": "text"}, {"input": "label"}],
                    "captures": [{"output": "out", "from": {"glob": "*.out"}}]
                }},
                "inputs": [{"name": "text", "type": {"file": {}}}, {"name": "label", "type": "string"}],
                "outputs": [{"name": "out", "type": {"file": {}}}]
            }],
            "edges": [
                edge((None, "text"), (Some("cat"), "text")),
                edge((None, "label"), (Some("cat"), "label")),
                edge((Some("cat"), "out"), (None, "out"))
            ]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("map.wir.json"), document.to_string()).unwrap();
    let texts: Vec<Json> = (0..texts)
        .map(|index| {
            std::fs::write(root.join(format!("{index}.txt")), "text\n").unwrap();
            json!({"class": "File", "location": format!("{index}.txt")})
        })
        .collect();
    let labels: Vec<String> = (0..labels).map(|index| format!("l{index}")).collect();
    let inputs = json!({"texts": texts, "labels": labels});
    std::fs::write(root.join("inputs.json"), inputs.to_string()).unwrap();
    Launch {
        target: root.join("map.wir.json"),
        inputs: Some(root.join("inputs.json")),
        scratch: root.join("scratch"),
        output_dir: Some(root.join("out")),
        ..Launch::default()
    }
}

/// Writes `<last word of the command>.out` for each task, and records the tasks.
struct Echoing {
    root: std::path::PathBuf,
    names: Vec<(String, String)>,
}

impl Executor for Echoing {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.names.push((task.name.clone(), task.step.clone()));
        let work = self.root.join(format!("work-{}", task.id));
        std::fs::create_dir_all(&work).unwrap();
        let path = format!("{}.out", task.command.last().unwrap());
        std::fs::write(work.join(&path), "").unwrap();
        let files = vec![OutputFile {
            location: work.join(&path).to_string_lossy().into_owned(),
            path,
            ..OutputFile::default()
        }];
        let outcome = Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        };
        Running::ended(Ok(outcome))
    }
}

/// Runs a map; the names and steps of its tasks, and the label of each output in its place.
fn run(mode: &str, texts: usize, labels: usize) -> Result<(Vec<(String, String)>, Json), String> {
    let folder = tempfile::tempdir().unwrap();
    let launch = document(folder.path(), mode, texts, labels);
    let settings = Settings {
        print: false,
        ..Settings::default()
    };
    let mut driver = WirDriver::new(&launch, settings).unwrap();
    let mut executor = Echoing {
        root: folder.path().to_owned(),
        names: Vec::new(),
    };
    let execution = execute("map", &mut driver, &mut executor);
    execution.into_result().map_err(|error| error.to_string())?;
    let outs = driver.results().unwrap()["outs"].clone();
    Ok((executor.names, label(&outs)))
}

/// The value with each File replaced by the label in its name.
fn label(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(label).collect()),
        file => json!(file["basename"].as_str().unwrap().trim_end_matches(".out")),
    }
}

#[test]
fn runs_dot_rounds() {
    let (names, outs) = run("dot", 2, 2).unwrap();
    let first = ("each/0/cat".to_owned(), "each/cat".to_owned());
    assert_eq!(
        names[0], first,
        "a task keeps its step without round numbers"
    );
    assert_eq!(names.len(), 2);
    assert_eq!(outs, json!(["l0", "l1"]));
}

#[test]
fn runs_cross_rounds() {
    let (names, outs) = run("flat_cross", 2, 3).unwrap();
    assert_eq!(names.len(), 6);
    assert_eq!(
        outs,
        json!(["l0", "l1", "l2", "l0", "l1", "l2"]),
        "the first input varies slowest"
    );
    let (_, outs) = run("nested_cross", 2, 3).unwrap();
    assert_eq!(outs, json!([["l0", "l1", "l2"], ["l0", "l1", "l2"]]));
    let (names, outs) = run("nested_cross", 2, 0).unwrap();
    assert_eq!((names.len(), outs), (0, json!([[], []])));
}

#[test]
fn handles_empty_inputs() {
    let (names, outs) = run("dot", 0, 0).unwrap();
    assert_eq!((names.len(), outs), (0, json!([])));
}

#[test]
fn refuses_unequal_lengths() {
    let error = run("dot", 2, 3).unwrap_err();
    assert_eq!(error, "op each: the inputs of a dot map have lengths 2, 3");
}
