//! nf-gadi against the plugin 1.4.0 with Nextflow 26.04.6: the events of the recorded probe runs
//! give the recorded CSV, resumed CSV and JSON reports.
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

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_gadi");
    std::fs::read_to_string(path.join(name)).expect("reads the fixture")
}

/// The observer of the probe config with this `gadi` scope.
fn activate(launch: &Path, scope: &str) -> Box<dyn Observer> {
    let file = launch.join("nextflow.config");
    let text = format!("plugins {{ id 'nf-gadi@1.4.0' }}\ngadi {{\n{scope}\n}}\n");
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

/// A task of the probe whose job was the `n`th, with the summary that the fake `qsub` wrote,
/// which the PBS executor adds to the end of its standard error.
fn task(name: &str, n: u32, cached: bool) -> Event {
    let summary = format!(
        "======================================================================================\n\
         \x20                 Resource Usage on 2026-11-06 14:10:0{n}:\n\
         \x20  Job Id:             {}.gadi-pbs\n\
         \x20  Project:            ab12\n\
         \x20  Exit Status:        0\n\
         \x20  Service Units:      0.0{n}\n\
         \x20  NCPUs Requested:    {}                      NCPUs Used: {}\n\
         \x20                                          CPU Time Used: 00:00:0{n}\n\
         \x20  Memory Requested:   {n}.0GB                 Memory Used: 1{n}.5MB\n\
         \x20  GPU Memory Used:    99.0GB\n\
         \x20  Walltime Requested: 01:00:00            Walltime Used: 00:00:0{}\n\
         \x20  JobFS Requested:    100.0MB                JobFS Used: 0B\n\
         ======================================================================================\n",
        140_000_000 + n,
        n * 2,
        n * 2,
        n + 1
    );
    let record =
        serde_json::json!({"task_id": n, "name": name, "process": name, "queue": "normal"});
    let task = Task {
        record: record.as_object().cloned().unwrap_or_default(),
        process_id: Some(n),
        hash: String::new(),
        executor: "pbspro".into(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        values: Vec::new(),
        declared: Vec::new(),
        stdout: String::new(),
        stderr: summary,
    };
    match cached {
        true => Event::TaskCached(task),
        false => Event::TaskCompleted(task),
    }
}

/// Runs the events of one probe run in `launch`.
fn run(launch: &Path, scope: &str, cached: bool) {
    let mut observer = activate(launch, scope);
    let start = Start {
        run_name: "probe".into(),
        session_id: "1cb210cc-1e34-4816-9ece-f9d7d7489783".into(),
        launch_dir: launch.to_string_lossy().into_owned(),
        work_dir: String::new(),
        project_dir: launch.to_string_lossy().into_owned(),
        script: launch.join("main.nf").to_string_lossy().into_owned(),
        project_name: "gadi-probe".into(),
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
        resume: cached,
    };
    observer.notify(&Event::RunStarted(Box::new(start)));
    observer.notify(&task("BIG", 1, cached));
    observer.notify(&task("SMALL", 2, cached));
    observer.notify(&Event::RunEnded(End {
        success: true,
        error: None,
        counts: Counts::default(),
        task: None,
        outputs: Vec::new(),
        end: 2_000,
    }));
}

#[test]
fn writes_recorded_reports() {
    let folder = tempfile::tempdir().expect("a folder");
    let launch = folder.path();
    let csv = "format = 'csv'\noutput = 'report.csv'\npublishTo = 'published/usage.csv'";
    run(launch, csv, false);
    let written = std::fs::read_to_string(launch.join("report.csv")).expect("a report");
    assert_eq!(written, fixture("report.csv"));
    let published = std::fs::read_to_string(launch.join("published/usage.csv")).expect("a copy");
    assert_eq!(published, written);
    // A resumed run writes the report anew, with the cached tasks.
    run(launch, csv, true);
    let written = std::fs::read_to_string(launch.join("report.csv")).expect("a report");
    assert_eq!(written, fixture("report-resume.csv"));
    run(launch, "format = 'json'\noutput = 'report.json'", false);
    let written = std::fs::read_to_string(launch.join("report.json")).expect("a report");
    assert_eq!(written, fixture("report.json"));
}
