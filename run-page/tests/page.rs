//! Starts the page on a small recorded run and reads what the script of the page reads: the
//! runs, one run, a task with its logs, a download, and the stream of live updates.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;
use std::path::Path;

use run_database::{Database, RunStart};
use run_page::{Server, Settings};
use serde_json::Value;
use workflow_graph::{Input, Language, Outcome, OutputFile, Task, TaskId};

/// Records a finished run of one task that read `in.txt` and wrote `out.txt`, and a second run
/// that is still going on.
fn record(directory: &Path) -> std::path::PathBuf {
    let input = directory.join("in.txt");
    std::fs::write(&input, "raw\n").expect("writes the input");
    let output = directory.join("out.txt");
    std::fs::write(&output, "RAW\n").expect("writes the output");
    let path = directory.join("runs.sqlite");
    let start = |id: &str| RunStart {
        id: id.into(),
        resumes: None,
        workflow: "/work/Snakefile".into(),
        language: Language::Snakemake,
        revision: None,
        arguments: vec!["run".into(), "Snakefile".into()],
        parameters: Vec::new(),
        executor: "simulator".into(),
        simulated: true,
        store: None,
    };
    let task = Task {
        id: TaskId(4),
        name: "upper (a)".into(),
        step: "upper".into(),
        command: vec![
            "bash".into(),
            "-c".into(),
            "tr a-z A-Z < in.txt > out.txt".into(),
        ],
        inputs: vec![Input {
            path: "in.txt".into(),
            source: input.to_string_lossy().into_owned(),
            directory: false,
            content: None,
        }],
        outputs: vec!["out.txt".into()],
        ..Task::default()
    };
    let outcome = Outcome {
        id: TaskId(4),
        exit_code: 0,
        files: vec![OutputFile {
            path: "out.txt".into(),
            location: output,
            directory: false,
        }],
        stdout: "done\n".into(),
        stderr: String::new(),
    };
    let mut recorder = Database::open(&path)
        .expect("opens")
        .record(&start("finished-run"))
        .expect("starts");
    recorder.requested(&task, &[]);
    recorder.started(TaskId(4), 1);
    recorder.finished(&outcome, 1, false);
    recorder.ended(None);
    recorder.close().expect("closes");
    let recorder = Database::open(&path)
        .expect("opens")
        .record(&start("running-run"))
        .expect("starts");
    // The run has not ended, and its process, this test, still runs.
    drop(recorder);
    path
}

fn get(url: &str) -> (u16, String) {
    let response = reqwest::blocking::get(url).expect("the page answers");
    let status = response.status().as_u16();
    (status, response.text().expect("a text"))
}

fn get_json(url: &str) -> Value {
    let (status, text) = get(url);
    assert_eq!(status, 200, "{url}: {text}");
    serde_json::from_str(&text).expect("JSON")
}

#[test]
fn the_page_shows_runs_tasks_logs_and_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let database = record(directory.path());
    let server = Server::start(&Settings::new(database)).expect("starts");
    let url = server.url().to_owned();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");

    let runs = get_json(&format!("{url}/api/runs"));
    assert_eq!(runs.as_array().map(Vec::len), Some(2));
    let run = get_json(&format!("{url}/api/runs/finished-run"));
    assert_eq!(run["run"]["state"], "succeeded");
    assert_eq!(run["run"]["simulated"], true);
    assert_eq!(run["tasks"][0]["name"], "upper (a)");
    assert_eq!(run["sequence"], 4);

    let task = get_json(&format!("{url}/api/runs/finished-run/tasks/4"));
    assert_eq!(task["row"]["state"], "succeeded");
    assert_eq!(task["stdout"]["text"], "done\n");
    assert_eq!(task["stderr"], Value::Null);
    assert_eq!(task["outputs"][0]["path"], "out.txt");
    let (status, content) = get(&format!("{url}/api/runs/finished-run/tasks/4/outputs/0"));
    assert_eq!((status, content.as_str()), (200, "RAW\n"));
    let (status, log) = get(&format!(
        "{url}/api/runs/finished-run/tasks/4/logs/1/stdout"
    ));
    assert_eq!((status, log.as_str()), (200, "done\n"));
    let (status, _) = get(&format!("{url}/api/runs/finished-run/tasks/4/outputs/1"));
    assert_eq!(status, 404);
    let (status, _) = get(&format!("{url}/api/runs/no-such-run"));
    assert_eq!(status, 404);

    // The stream of a finished run sends one update with the whole state and ends.
    let mut response =
        reqwest::blocking::get(format!("{url}/api/runs/finished-run/events?after=0"))
            .expect("the stream answers");
    let mut stream = String::new();
    response
        .read_to_string(&mut stream)
        .expect("reads the stream");
    assert!(stream.contains("event: update"), "{stream}");
    assert!(stream.contains("id: 4"), "{stream}");
    let data = stream
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("a data line");
    let update: Value = serde_json::from_str(data).expect("JSON");
    assert_eq!(update["run"]["state"], "succeeded");
    assert_eq!(update["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(update["graph"], true);

    server.stop();
}
