//! Prints the wir of WDL documents with `uwe lower`, and runs one natively and with `-lowered`
//! on the simulator with a WDL input file.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_database::Database;
use serde_json::Value as Json;

mod common;

const WORKFLOW: &str = r#"version 1.1
workflow check {
  input {
    File text
    Array[String] names
  }
  scatter (name in names) {
    call greet { input: name = name, text = text }
  }
  output {
    Array[File] greetings = greet.out
  }
}

task greet {
  input {
    String name
    File text
  }
  command <<<
    cat ~{text} > "~{name}.txt"
  >>>
  output {
    File out = "~{name}.txt"
  }
  runtime {
    docker: "ubuntu:24.04"
  }
}
"#;

/// Runs the binary in `directory` with the cache folder `cache`; its exit code, what it
/// printed on standard output, and on standard error.
fn uwe(directory: &Path, cache: &Path, arguments: &[&str]) -> (i32, String, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .expect("runs uwe");
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let code = output.status.code().unwrap_or(-1);
    (code, text(&output.stdout), text(&output.stderr))
}

/// The language of the latest run and the images of its tasks, by step.
fn latest(cache: &Path) -> (String, Vec<(String, Option<String>)>) {
    let database = Database::open(&cache.join("uwe/runs.sqlite")).expect("opens");
    let run = database.runs(1).expect("reads").remove(0);
    let mut tasks = Vec::new();
    for row in database.tasks(&run.id).expect("reads the tasks") {
        let detail = (database.task(&run.id, row.id).expect("reads")).expect("a task");
        tasks.push((detail.description.step, detail.description.image));
    }
    tasks.sort();
    (run.language, tasks)
}

#[test]
fn prints_lowered_documents() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    std::fs::write(root.join("check.wdl"), WORKFLOW).expect("writes the document");
    let (code, printed, errors) = uwe(root, &cache, &["lower", "check.wdl"]);
    assert_eq!((code, errors.as_str()), (0, ""), "{printed}");
    let document: Json = serde_json::from_str(&printed).expect("prints JSON");
    assert_eq!(document["sources"][0]["location"], "check.wdl");
    assert_eq!(
        document["graphs"][0]["ops"][0]["kind"]["map"]["over"][0],
        "name"
    );
    let read = WORKFLOW.replace(
        r#"File out = "~{name}.txt""#,
        "String out = read_string(stdout())",
    );
    let read = read.replace("Array[File] greetings", "Array[String] greetings");
    std::fs::write(root.join("read.wdl"), read).expect("writes the document");
    let (code, _, errors) = uwe(root, &cache, &["lower", "read.wdl"]);
    assert_eq!(code, 0, "{errors}");
    let kept = "uwe: op scatter.greet/greet stays a native WDL op: output out reads files that \
                the task makes\n";
    assert_eq!(errors, kept);
}

#[test]
fn matches_native_tasks() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let (root, cache) = (folder.path(), folder.path().join("cache"));
    std::fs::write(root.join("check.wdl"), WORKFLOW).expect("writes the document");
    std::fs::write(root.join("text.txt"), "hi\n").expect("writes the text");
    let inputs = r#"{"check.text": "text.txt", "check.names": ["ada", "bob"]}"#;
    std::fs::write(root.join("inputs.json"), inputs).expect("writes the inputs");
    let native = ["run", "-executor", "simulator", "check.wdl", "inputs.json"];
    let (code, _, errors) = uwe(root, &cache, &native);
    assert_eq!(code, 0, "{errors}");
    let (language, alone) = latest(&cache);
    assert_eq!(language, "wdl");
    let lowered: Vec<&str> = native.iter().copied().chain(["-lowered"]).collect();
    let (code, _, errors) = uwe(root, &cache, &lowered);
    assert_eq!(code, 0, "{errors}");
    let (language, through) = latest(&cache);
    assert_eq!(language, "wir");
    assert_eq!(alone.len(), 2, "a task per name");
    let images = |tasks: &[(String, Option<String>)]| -> Vec<Option<String>> {
        tasks.iter().map(|(_, image)| image.clone()).collect()
    };
    assert_eq!(images(&alone), images(&through));
    let steps: Vec<&str> = through.iter().map(|(step, _)| step.as_str()).collect();
    assert_eq!(
        steps,
        ["scatter.greet/greet/greet", "scatter.greet/greet/greet"]
    );
}
