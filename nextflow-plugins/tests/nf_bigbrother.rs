//! nf-bigbrother against the files that nf-bigbrother 1.1.0 wrote with Nextflow 26.04.6 for
//! the same pipelines; the run is named after the recorded session, as uwe's session is its run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

const SESSION: &str = "e92c6795-29ce-4a58-96f2-d46eb3f1e8d5";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(common::fixtures("nf_bigbrother").join(name)).expect("a fixture")
}

/// Runs `script` as `main.nf` with the recorded config, and gives the folder of the files.
fn run(folder: &Path, script: &str, success: bool) -> std::path::PathBuf {
    let fixtures = common::fixtures("nf_bigbrother");
    std::fs::copy(fixtures.join(script), folder.join("main.nf")).expect("copies the script");
    common::copy(&fixtures, folder, &["input.txt"]);
    let output = folder.join("bigbrother");
    let config = format!(
        "plugins {{ id 'nf-bigbrother@1.1.0' }}\nbigbrother {{\n    outputDir = '{}'\n    \
         emitPartials = true\n    snapshotEvery = 2\n}}\n",
        output.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, SESSION);
    assert_eq!(
        execution.error.is_none(),
        success,
        "{:?}\n{printed}",
        execution.error
    );
    output
}

/// The keys of the objects at each indent of a pretty-printed JSON text, in order and once.
fn keys(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some((key, _)) = trimmed
            .strip_prefix('"')
            .and_then(|rest| rest.split_once("\":"))
        {
            let key = format!("{}{key}", line.len() - trimmed.len());
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn name(path: &Value) -> String {
    let path = path.as_str().unwrap_or_default();
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// The tasks of a snapshot by name: their parents' and children's names and their files.
fn graph(snapshot: &Value) -> BTreeSet<String> {
    let tasks = snapshot["workflow"]["specification"]["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let named = |id: &Value| {
        let task = tasks.iter().find(|task| task["id"] == *id);
        task.map(|task| task["name"].as_str().unwrap_or_default().to_owned())
            .unwrap_or_default()
    };
    (tasks.iter())
        .map(|task| {
            let names = |key: &str| -> Vec<String> {
                let mut names: Vec<String> = task[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(named)
                    .collect();
                names.sort();
                names
            };
            let files = |key: &str| -> Vec<String> {
                task[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(name)
                    .collect()
            };
            format!(
                "{} {:?} {:?} {:?} {:?}",
                task["name"].as_str().unwrap_or_default(),
                names("parents"),
                names("children"),
                files("inputFiles"),
                files("outputFiles")
            )
        })
        .collect()
}

#[test]
fn writes_snapshots() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let output = run(directory.path(), "main.nf", true);
    let mut names: Vec<String> = std::fs::read_dir(&output)
        .expect("the files")
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    let expected: Vec<String> = ["complete", "partial_000", "partial_001"]
        .iter()
        .flat_map(|phase| ["dot", "json"].map(|kind| format!("{phase}_main.nf_{SESSION}.{kind}")))
        .chain([format!("events_main.nf_{SESSION}.jsonl")])
        .collect();
    let mut expected = expected;
    expected.sort();
    assert_eq!(names, expected);
    let read = |phase: &str, kind: &str| {
        std::fs::read_to_string(output.join(format!("{phase}_main.nf_{SESSION}.{kind}")))
            .expect("the file")
    };
    let text = read("complete", "json");
    let recorded = fixture("complete.json");
    assert_eq!(keys(&text), keys(&recorded), "the keys in order");
    let (written, recorded): (Value, Value) = (
        serde_json::from_str(&text).expect("JSON"),
        serde_json::from_str(&recorded).expect("JSON"),
    );
    assert_eq!(graph(&written), graph(&recorded));
    let sizes = |snapshot: &Value| -> BTreeSet<String> {
        let files = snapshot["workflow"]["specification"]["files"]
            .as_array()
            .cloned();
        (files.unwrap_or_default().iter())
            .map(|file| format!("{} {}", name(&file["id"]), file["sizeInBytes"]))
            .collect()
    };
    assert_eq!(sizes(&written), sizes(&recorded));
    let task = &written["workflow"]["execution"]["tasks"][0];
    assert_eq!(task["bigbrother"]["identity"]["process"], "SPLIT");
    assert_eq!(task["bigbrother"]["outcome"]["exitStatus"], "0");
    assert_eq!(task["bigbrother"]["placement"]["executor"], "local");
    assert!(
        task["executedAt"]
            .as_str()
            .is_some_and(|time| time.ends_with('Z'))
    );
    let partial: Value = serde_json::from_str(&read("partial_000", "json")).expect("JSON");
    assert_eq!(partial["workflow"]["execution"]["makespanInSeconds"], 0.0);
    let dot = read("complete", "dot");
    let lines = |text: &str| -> BTreeSet<String> { text.lines().map(str::to_owned).collect() };
    assert_eq!(lines(&dot), lines(&fixture("complete.dot")), "{dot}");
    let events = read("events", "jsonl");
    let kinds = |text: &str| -> Vec<String> {
        (text.lines())
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .map(|line| line["event"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    assert_eq!(kinds(&events), kinds(&fixture("events.jsonl")));
    let first: Value =
        serde_json::from_str(events.lines().next().unwrap_or_default()).expect("JSON");
    let recorded: Value =
        serde_json::from_str(fixture("events.jsonl").lines().next().unwrap_or_default())
            .expect("JSON");
    assert_eq!(first["runtimeSystem"], recorded["runtimeSystem"]);
}

#[test]
fn writes_errors() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let output = run(directory.path(), "fail.nf", false);
    for phase in ["error", "complete"] {
        assert!(
            output
                .join(format!("{phase}_main.nf_{SESSION}.json"))
                .is_file(),
            "{phase}"
        );
    }
    let events = std::fs::read_to_string(output.join(format!("events_main.nf_{SESSION}.jsonl")))
        .expect("the events");
    let kinds: Vec<String> = (events.lines())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|line| line["event"].as_str().unwrap_or_default().to_owned())
        .collect();
    // Nextflow 26.04.6 ended the recorded run twice; uwe ends it once.
    assert_eq!(kinds, ["run", "task", "error", "complete"]);
    let recorded = fixture("fail-events.jsonl");
    let task: Value =
        serde_json::from_str(events.lines().nth(1).unwrap_or_default()).expect("JSON");
    let theirs: Value =
        serde_json::from_str(recorded.lines().nth(1).unwrap_or_default()).expect("JSON");
    assert_eq!(task["bigbrother"], Value::Null);
    let outcome = &task["execution"]["bigbrother"]["outcome"];
    assert_eq!(
        outcome["exitStatus"],
        theirs["execution"]["bigbrother"]["outcome"]["exitStatus"]
    );
    assert_eq!(
        outcome["status"],
        theirs["execution"]["bigbrother"]["outcome"]["status"]
    );
}
