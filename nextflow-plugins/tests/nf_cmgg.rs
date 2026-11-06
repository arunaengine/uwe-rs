//! nf-cmgg against the plugin 0.5.0 with Nextflow 26.04.6: the run observers hear the events of
//! the recorded probe runs of `fixtures/nf_cmgg` and write `DONE` and the sample sheets.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::observer::{Counts, End, Event, Observer, Start};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_cmgg")
}

/// The observers of the probe config with `extra` settings, in a launch folder with the
/// worksheets and sample sheets of the probe.
fn activate(launch: &Path, extra: &str) -> Vec<Box<dyn Observer>> {
    for name in [
        "worksheet.yml",
        "bad-worksheet.yml",
        "samples.csv",
        "samples-run1.csv",
    ] {
        std::fs::copy(fixtures().join(name), launch.join(name)).expect("copies");
    }
    let file = launch.join("nextflow.config");
    let text = format!(
        "plugins {{ id 'nf-cmgg@0.5.0' }}\nmanifest.name = 'probe/pipe'\ncmgg {{\n    \
         done.enabled = true\n    samplesheets {{\n        enabled = true\n        \
         worksheets = ['worksheet.yml']\n    }}\n}}\n{extra}\n"
    );
    std::fs::write(&file, text).expect("writes");
    let settings = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: launch.to_string_lossy().into_owned(),
        launch_dir: launch.to_string_lossy().into_owned(),
        work_dir: launch.join("work").to_string_lossy().into_owned(),
    };
    let config = nextflow_config::load(&[file], &settings, &mut Quiet).expect("loads");
    nextflow_plugins::catalogue::observers(&config).expect("activates")
}

/// Runs the events of the probe: each sample publishes its BAM and reads, then the metrics.
fn run(
    launch: &Path,
    observers: &mut [Box<dyn Observer>],
    input: &str,
    outdir: &str,
    success: bool,
) {
    let start = Start {
        run_name: "probe".into(),
        session_id: "probe".into(),
        launch_dir: launch.to_string_lossy().into_owned(),
        work_dir: launch.join("work").to_string_lossy().into_owned(),
        project_dir: launch.to_string_lossy().into_owned(),
        script: "main.nf".into(),
        project_name: "probe".into(),
        repository: None,
        commit_id: None,
        revision: None,
        command_line: "nextflow run main.nf".into(),
        profile: "standard".into(),
        config_files: Vec::new(),
        params: serde_json::json!({"outdir": outdir, "input": input, "min_reads": 100}),
        config: serde_json::json!({}),
        manifest: serde_json::json!({}),
        processes: Vec::new(),
        start: 0,
    };
    let mut events = vec![Event::RunStarted(Box::new(start))];
    let results = launch.join("results");
    std::fs::create_dir_all(&results).expect("creates");
    for id in ["S1", "S2", "S10", "X9"] {
        for name in [
            format!("{id}.bam"),
            format!("{id}_R1.fastq.gz"),
            format!("{id}_R2.fastq.gz"),
        ] {
            std::fs::write(results.join(&name), "x").expect("writes");
            events.push(published(&results.join(name)));
        }
    }
    let metrics = "Sample\tyield\nS1\t150\nS2\t50\nS10\t300\nS99\t1\n";
    std::fs::write(results.join("metrics.tsv"), metrics).expect("writes");
    events.push(published(&results.join("metrics.tsv")));
    events.push(Event::RunEnded(End {
        success,
        error: None,
        counts: Counts::default(),
        task: None,
        outputs: Vec::new(),
        end: 0,
    }));
    for event in &events {
        for observer in observers.iter_mut() {
            observer.notify(event);
        }
    }
}

fn published(target: &Path) -> Event {
    Event::FilePublished {
        source: "/work/x".into(),
        target: target.to_string_lossy().into_owned(),
    }
}

/// The files below a folder, each as the recording lists it: `--- ./<path>`, its text, `[end]`.
fn listing(folder: &Path, launch: &Path) -> String {
    let mut files = Vec::new();
    let mut pending = vec![folder.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("lists") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut text = String::new();
    for file in files {
        let relative = file.strip_prefix(folder).expect("inside");
        let content = std::fs::read_to_string(&file).expect("reads");
        let content = content.replace(&launch.to_string_lossy().into_owned(), "<probe-dir>");
        text.push_str(&format!("--- ./{}\n{content}[end]\n", relative.display()));
    }
    text
}

#[test]
fn writes_recorded_sheets() {
    for (input, expected) in [
        ("samples-run1.csv", "run1-samplesheets.txt"),
        ("samples.csv", "run2-samplesheets.txt"),
    ] {
        let launch = tempfile::tempdir().expect("a folder");
        let mut observers = activate(launch.path(), "");
        run(launch.path(), &mut observers, input, "results", true);
        let sheets = launch.path().join("results/samplesheets");
        let expected = std::fs::read_to_string(fixtures().join(expected)).expect("reads");
        assert_eq!(listing(&sheets, launch.path()), expected, "{input}");
        let done = std::fs::read_to_string(launch.path().join("results/DONE")).expect("DONE");
        let (day, time) = done
            .strip_prefix("Pipeline completed on ")
            .and_then(|rest| rest.split_once(" at "))
            .expect("the DONE text");
        assert_eq!(
            (day.len(), &day[2..3], time.len(), &time[2..3]),
            (10, "/", 8, ":")
        );
    }
}

#[test]
fn follows_settings() {
    let launch = tempfile::tempdir().expect("a folder");
    let extra = "cmgg.done.location = 'done-here'\ncmgg.samplesheets.location = 'sheets-here'";
    let mut observers = activate(launch.path(), extra);
    run(
        launch.path(),
        &mut observers,
        "samples.csv",
        "results",
        true,
    );
    assert!(launch.path().join("done-here/DONE").is_file());
    assert!(launch.path().join("sheets-here/wgs/all.yaml").is_file());
    assert!(!launch.path().join("results/DONE").exists());

    let launch = tempfile::tempdir().expect("a folder");
    let extra = "cmgg.done.enabled = 'yes'\nmanifest.name = 'other/pipe'";
    let mut observers = activate(launch.path(), extra);
    run(
        launch.path(),
        &mut observers,
        "samples.csv",
        "results",
        true,
    );
    assert!(
        !launch.path().join("results/DONE").exists(),
        "a text is no boolean"
    );
    assert!(
        !launch.path().join("results/samplesheets").exists(),
        "no worksheet"
    );

    let launch = tempfile::tempdir().expect("a folder");
    let mut observers = activate(launch.path(), "");
    run(
        launch.path(),
        &mut observers,
        "samples.csv",
        "results",
        false,
    );
    assert!(!launch.path().join("results/DONE").exists(), "a failed run");

    let launch = tempfile::tempdir().expect("a folder");
    let extra = "cmgg.samplesheets.worksheets = ['bad-worksheet.yml', 'missing.yml']";
    let mut observers = activate(launch.path(), extra);
    run(launch.path(), &mut observers, "samples.csv", "", true);
    assert!(
        launch.path().join("work/DONE").is_file(),
        "no outdir: the work folder"
    );
    assert!(
        !launch.path().join("work/samplesheets").exists(),
        "an invalid worksheet"
    );
}
