//! nf-runinsights against the plugin 0.2.0 with Nextflow 26.04.6: the events of the two recorded
//! probe runs give the recorded history files and reports, the second with its regression.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::observer::{Counts, End, Event, Observer, Start, Task};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_runinsights")
}

/// The observer of the probe config, with the history in `launch/history`.
fn activate(launch: &Path) -> Box<dyn Observer> {
    let file = launch.join("nextflow.config");
    let text = format!(
        "plugins {{ id 'nf-runinsights@0.2.0' }}\n\
         runinsights {{ history = '{}/history' }}\n",
        launch.display()
    );
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: launch.to_string_lossy().into_owned(),
        launch_dir: launch.to_string_lossy().into_owned(),
        work_dir: launch.join("work").to_string_lossy().into_owned(),
    };
    let config = nextflow_config::load(&[file], &options, &mut Quiet).expect("loads");
    let mut observers = nextflow_plugins::catalogue::observers(&config).expect("activates");
    observers.pop().expect("an observer")
}

/// A completed CRUNCH task with the times of the probe.
fn task(number: u64, submit: i64, start: i64, realtime: i64) -> Event {
    let record = serde_json::json!({
        "task_id": number,
        "name": format!("CRUNCH ({number})"),
        "process": "CRUNCH",
        "status": "COMPLETED",
        "exit": 0,
        "attempt": 1,
        "submit": submit,
        "start": start,
        "realtime": realtime,
        "cpus": 1,
        "memory": 2147483648u64,
        "%cpu": null,
        "peak_rss": null,
        "read_bytes": null,
        "container": null,
    });
    Event::TaskCompleted(Task {
        record: record.as_object().cloned().unwrap_or_default(),
        process_id: Some(1),
        hash: String::new(),
        executor: "local".into(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        values: Vec::new(),
        declared: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
    })
}

/// Runs the events of one probe run in `launch` and returns its report, without its time, and
/// its history file, without its time.
fn run(
    launch: &Path,
    name: &str,
    session: &str,
    realtimes: [i64; 2],
) -> (String, serde_json::Value) {
    let mut observer = activate(launch);
    let start = Start {
        run_name: name.into(),
        session_id: session.into(),
        launch_dir: launch.to_string_lossy().into_owned(),
        work_dir: String::new(),
        project_dir: launch.join("ri").to_string_lossy().into_owned(),
        script: launch.join("ri/main.nf").to_string_lossy().into_owned(),
        project_name: "ri".into(),
        repository: None,
        commit_id: None,
        revision: None,
        command_line: String::new(),
        profile: "standard".into(),
        config_files: Vec::new(),
        params: serde_json::json!({}),
        config: serde_json::json!({}),
        manifest: serde_json::json!({}),
        processes: Vec::new(),
        start: 1_000,
        resume: false,
    };
    observer.notify(&Event::RunStarted(Box::new(start)));
    observer.notify(&task(2, 1_000, 1_035, realtimes[0]));
    observer.notify(&task(1, 1_000, 1_040, realtimes[1]));
    observer.notify(&Event::RunEnded(End {
        success: true,
        error: None,
        counts: Counts::default(),
        task: None,
        outputs: Vec::new(),
        end: 1_762_422_309_841,
    }));
    let report = std::fs::read_to_string(launch.join("runinsights-report.md")).expect("a report");
    let entry = std::fs::read_dir(launch.join("history"))
        .expect("a history")
        .flatten();
    let file = entry
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(&format!("-{name}.json")))
        .expect("the run's history file");
    (without_times(&report), read(&file))
}

fn without_times(report: &str) -> String {
    let lines = report
        .lines()
        .filter(|line| !line.starts_with("- **Time:**"));
    lines.map(|line| format!("{line}\n")).collect()
}

fn read(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("reads");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    value["ts"] = serde_json::Value::Null;
    value
}

#[test]
fn writes_recorded_reports() {
    let launch = tempfile::tempdir().expect("a folder");
    let launch = launch.path();
    let fixture = |name: &str| std::fs::read_to_string(fixtures().join(name)).expect("reads");
    let first = run(
        launch,
        "spontaneous_goldwasser",
        "1cb210cc-1e34-4816-9ece-f9d7d7489783",
        [981, 973],
    );
    assert_eq!(first.0, without_times(&fixture("report-run1.md")));
    assert_eq!(first.1, read(&fixtures().join("run1.json")));
    let second = run(
        launch,
        "goofy_sinoussi",
        "86c39c30-1c2c-480a-835c-e253fecaabc0",
        [3986, 3978],
    );
    assert_eq!(second.0, without_times(&fixture("report-run2.md")));
    let mut recorded = read(&fixtures().join("run2.json"));
    // The probe's second run waited 33 ms on average; the events here wait as the first did.
    recorded["processes"]["CRUNCH"]["queue_ms_median"] = 37.5.into();
    assert_eq!(second.1, recorded);
}
