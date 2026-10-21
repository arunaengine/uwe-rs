//! Starts the page on a small recorded run and reads what the script of the page reads: the
//! runs, one run, a task with its logs, a download, and the stream of live updates.
// Copyright (c) 2026 The uwe Contributors
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
    record_with_address(directory, None)
}

fn record_with_address(directory: &Path, address: Option<&str>) -> std::path::PathBuf {
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
        arguments: ["run".to_owned(), "Snakefile".to_owned()]
            .into_iter()
            .chain(address.map(str::to_owned))
            .collect(),
        parameters: Vec::new(),
        executor: "simulator".into(),
        simulated: true,
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    let mut task = Task {
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
            ..Default::default()
        }],
        outputs: vec!["out.txt".into()],
        ..Task::default()
    };
    if let Some(address) = address {
        task.inputs[0].source = address.to_owned();
        task.script = Some(format!("curl '{address}'"));
    }
    let mut outcome = Outcome {
        id: TaskId(4),
        exit_code: 0,
        files: vec![OutputFile {
            path: "out.txt".into(),
            location: output.to_string_lossy().into_owned(),
            ..OutputFile::default()
        }],
        stdout: "done\n".into(),
        reproducibility: Some(workflow_graph::ReproducibilityCheck {
            earlier_run: "earlier-run".into(),
            verdict: workflow_graph::Verdict::Differs {
                paths: vec!["out.txt".into()],
            },
        }),
        ..Outcome::default()
    };
    if let Some(address) = address {
        outcome.stdout = format!("read {address}");
    }
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
fn shows_run_details() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let database = record(directory.path());
    let server = Server::start(&Settings::new(database)).expect("starts");
    let url = server.url().to_owned();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");

    let (status, page) = get(&format!("{url}/runs/finished-run/tasks/4"));
    assert_eq!(status, 200);
    assert!(page.contains("/page.js"), "{page}");
    let (status, script) = get(&format!("{url}/run.js"));
    assert_eq!(status, 200);
    assert!(script.contains("EventSource"));

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
    assert_eq!(task["checks"][0]["verdict"], "differs");
    assert_eq!(task["checks"][0]["earlier"], "earlier-run");
    assert!(script.contains("Reproducibility check"));
    let (status, svg) = get(&format!("{url}/api/runs/finished-run/graph.svg"));
    assert_eq!(status, 200, "{svg}");
    assert!(svg.contains("<svg") && svg.contains("id=\"t4\""), "{svg}");
    let (status, svg) = get(&format!("{url}/api/runs/finished-run/graph.svg?view=steps"));
    assert_eq!(status, 200, "{svg}");
    assert!(svg.contains("upper"), "{svg}");
    let (status, _) = get(&format!("{url}/api/runs/finished-run/graph.svg?view=files"));
    assert_eq!(status, 400);
    let (status, _) = get(&format!(
        "{url}/api/runs/finished-run/graph.svg?direction=up"
    ));
    assert_eq!(status, 400);
    let (status, svg) = get(&format!(
        "{url}/api/runs/finished-run/graph.svg?direction=right"
    ));
    assert_eq!(status, 200, "{svg}");
    assert!(svg.contains("data-direction=\"right\""), "{svg}");
    for (script, function) in [
        ("graph.js", "function loadGraph"),
        ("camera.js", "function fitGraph"),
        ("boxes.js", "function paintStates"),
    ] {
        let (status, text) = get(&format!("{url}/{script}"));
        assert_eq!(status, 200);
        assert!(text.contains(function), "{script}: {text}");
    }
    // No earlier run has the task, so there is nothing to compare.
    let why = get_json(&format!("{url}/api/runs/finished-run/tasks/4/why"));
    assert_eq!(why, Value::Null);
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

#[test]
fn folds_large_steps() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    let start = RunStart {
        id: "wide-run".into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: Vec::new(),
        parameters: Vec::new(),
        executor: "local".into(),
        simulated: false,
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    let mut recorder = Database::open(&path)
        .expect("opens")
        .record(&start)
        .expect("starts");
    for id in 1..=7 {
        let task = Task {
            id: TaskId(id),
            name: format!("FASTQC ({id})"),
            step: "FASTQC".into(),
            ..Task::default()
        };
        recorder.requested(&task, &[]);
    }
    recorder.started(TaskId(1), 1);
    for id in 2..=3 {
        recorder.started(TaskId(id), 1);
        let outcome = Outcome {
            id: TaskId(id),
            ..Outcome::default()
        };
        recorder.finished(&outcome, 1, false);
    }
    recorder.close().expect("closes");
    let server = Server::start(&Settings::new(path)).expect("starts");
    let url = format!("{}/api/runs/wide-run/graph.svg", server.url());
    let (status, svg) = get(&url);
    assert_eq!(status, 200, "{svg}");
    // Seven tasks are more than five: one box, which a task that runs makes running.
    assert!(
        svg.contains("class=\"node step running\" id=\"s0\" data-step=\"FASTQC\""),
        "{svg}"
    );
    assert!(
        svg.contains(">7 tasks: 2 succeeded, 1 running, 4 waiting</text>"),
        "{svg}"
    );
    let (status, svg) = get(&format!("{url}?open=FASTQC"));
    assert_eq!(status, 200, "{svg}");
    assert!(
        svg.contains("class=\"node task running\" id=\"t1\""),
        "{svg}"
    );
    assert!(
        svg.contains("class=\"node task waiting\" id=\"t7\""),
        "{svg}"
    );
    let (_, svg) = get(&format!("{url}?fold=0"));
    assert!(svg.contains("id=\"t7\""), "{svg}");
    server.stop();
}

#[test]
fn updates_running_page() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    let start = RunStart {
        id: "live-run".into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: Vec::new(),
        parameters: Vec::new(),
        executor: "simulator".into(),
        simulated: true,
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    let mut recorder = Database::open(&path)
        .expect("opens")
        .record(&start)
        .expect("starts");
    let task = |id: u64| Task {
        id: TaskId(id),
        name: format!("P ({id})"),
        step: "P".into(),
        ..Task::default()
    };
    recorder.requested(&task(1), &[]);
    let server = Server::start(&Settings::new(path)).expect("starts");
    let url = format!("{}/api/runs/live-run/events?after=0", server.url());
    // The stream stays open while the run goes on, so it is read on a thread of its own.
    let reader = std::thread::spawn(move || {
        let mut response = reqwest::blocking::get(url).expect("the stream answers");
        let mut stream = String::new();
        response
            .read_to_string(&mut stream)
            .expect("reads the stream");
        stream
    });
    std::thread::sleep(std::time::Duration::from_millis(1200));
    recorder.started(TaskId(1), 1);
    let outcome = Outcome {
        id: TaskId(1),
        ..Outcome::default()
    };
    recorder.finished(&outcome, 1, false);
    recorder.requested(&task(2), &[]);
    // Let the page see task 2 waiting before the run ends and stops it.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    recorder.ended(Some("stopped by the test"));
    recorder.close().expect("closes");
    let stream = reader.join().expect("the reader ends");
    let updates: Vec<Value> = stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).expect("JSON"))
        .collect();
    assert!(updates.len() >= 2, "{stream}");
    // The first update has the task that was asked for, the last one the end of the run.
    assert_eq!(updates[0]["tasks"][0]["state"], "waiting");
    let last = updates.last().expect("an update");
    assert_eq!(last["run"]["state"], "failed");
    let states: Vec<&Value> = updates
        .iter()
        .flat_map(|update| update["tasks"].as_array().into_iter().flatten())
        .map(|task| &task["state"])
        .collect();
    assert!(states.contains(&&Value::from("succeeded")), "{stream}");
    assert!(states.contains(&&Value::from("waiting")), "{stream}");
    let stopped = last["tasks"].as_array().into_iter().flatten();
    let stopped: Vec<&Value> = stopped.map(|task| &task["state"]).collect();
    assert_eq!(stopped, [&Value::from("stopped")], "{stream}");
    server.stop();
}

#[test]
fn redacts_public_passwords() {
    let root = tempfile::tempdir().expect("a directory");
    let address = "ftp://user:sentinel-private-password@[::1]:21?email=a:b@c#fragment@x";
    let path = record_with_address(root.path(), Some(address));
    let database = Database::open(&path).expect("reads private database");
    let private = database
        .task("finished-run", 4)
        .expect("reads task")
        .expect("a task");
    assert_eq!(private.description.inputs[0].source, address);
    assert!(
        private
            .description
            .script
            .expect("private command")
            .contains("sentinel-private-password")
    );
    assert_eq!(
        database
            .run("finished-run")
            .expect("reads run")
            .expect("a run")
            .arguments[2],
        address
    );
    let html = run_page::report(&database, "finished-run")
        .expect("report")
        .expect("a report");
    assert!(!html.contains("sentinel-private-password"));
    let server = Server::start(&Settings::new(path)).expect("starts page");
    for route in [
        "/api/runs",
        "/api/runs/finished-run",
        "/api/runs/finished-run/tasks/4",
        "/api/runs/finished-run/events",
        "/api/runs/finished-run/graph.svg",
        "/api/runs/finished-run/tasks/4/inputs/0",
    ] {
        let (_, body) = get(&format!("{}{route}", server.url()));
        assert!(!body.contains("sentinel-private-password"), "{route}");
    }
    let detail = get_json(&format!("{}/api/runs/finished-run/tasks/4", server.url()));
    assert_eq!(
        detail["description"]["inputs"][0]["source"],
        "ftp://user:[secret]@[::1]:21?email=a:b@c#fragment@x"
    );
    assert_eq!(
        detail["inputs"][0]["location"],
        detail["description"]["inputs"][0]["source"]
    );
    assert_eq!(
        detail["input_ids"][0],
        run_database::public_resource_id(address)
    );
    server.stop();
}
