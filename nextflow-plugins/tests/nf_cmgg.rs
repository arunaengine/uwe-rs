//! nf-cmgg against the plugin 0.5.0 with Nextflow 26.04.6: the run observers hear the events of
//! the recorded probe runs of `fixtures/nf_cmgg` and write `DONE` and the sample sheets.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::observer::{Counts, End, Event, Observer, Start};
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{
    Attempt, Engine, Executor, Files, Launch, LocalFiles, Outcome, OutputFile, Running,
};

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
    nextflow_plugins::catalogue::observers(&config, launch).expect("activates")
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
        resume: false,
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

#[test]
fn keeps_sheets_inside() {
    let launch = tempfile::tempdir().expect("a folder");
    let mut observers = activate(launch.path(), "");
    let absolute = launch.path().join("absolute");
    let samples = format!(
        "sample,tag,kind,reads\nS1,../../escape,DNA,5\nS2,{},RNA,9\nS10,wes,DNA,7\n",
        absolute.display()
    );
    std::fs::write(launch.path().join("samples.csv"), samples).expect("writes");
    run(
        launch.path(),
        &mut observers,
        "samples.csv",
        "results",
        true,
    );
    assert!(!launch.path().join("escape").exists(), "a .. value");
    assert!(!absolute.exists(), "an absolute value");
    let sheets = launch.path().join("results/samplesheets");
    assert!(sheets.join("wes/all.yaml").is_file(), "a plain value");
}

/// Writes the files that a task's script writes with `>`, in a folder of its own, as a
/// local executor would; `metrics.tsv` gets the yields of the probe.
struct Writer(PathBuf);

impl Executor for Writer {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let folder = self.0.join(task.id.to_string());
        std::fs::create_dir_all(&folder).expect("creates");
        let script = task.script.clone().unwrap_or_default();
        let names = script
            .split("> ")
            .skip(1)
            .filter_map(|rest| rest.split_whitespace().next());
        let mut files = Vec::new();
        for name in names {
            let text = match name {
                "metrics.tsv" => "Sample\tyield\nS1\t150\nS2\t50\nS10\t300\nS99\t1\n",
                _ => "x\n",
            };
            let location = folder.join(name);
            std::fs::write(&location, text).expect("writes");
            files.push(OutputFile {
                path: name.into(),
                location: location.to_string_lossy().into_owned(),
                ..OutputFile::default()
            });
        }
        let id = task.id;
        Running::ended(Ok(Outcome {
            id,
            files,
            ..Outcome::default()
        }))
    }

    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(LocalFiles))
    }
}

#[test]
fn hears_real_publishes() {
    let launch = tempfile::tempdir().expect("a folder");
    let folder = launch.path();
    // The driver launches in the current folder, so the probe names its files absolutely.
    let at = folder.to_string_lossy();
    let probe = format!(
        "plugins {{ id 'nf-cmgg@0.5.0' }}\nmanifest.name = 'probe/pipe'\n\
         params.outdir = '{at}/results'\nparams.input = '{at}/samples.csv'\n\
         params.min_reads = 100\ncmgg {{\n    done.enabled = true\n    samplesheets {{\n        \
         enabled = true\n        worksheets = ['{at}/worksheet.yml']\n    }}\n}}\n"
    );
    std::fs::write(folder.join("nextflow.config"), probe).expect("writes");
    for name in ["main.nf", "worksheet.yml", "samples.csv"] {
        std::fs::copy(fixtures().join(name), folder.join(name)).expect("copies");
    }
    let run = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Quiet),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&run, settings).expect("a driver");
    let execution = Engine::new(&mut Writer(folder.join("tasks"))).run("main.nf", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let sheets = folder.join("results/samplesheets");
    let expected =
        std::fs::read_to_string(fixtures().join("run2-samplesheets.txt")).expect("reads");
    assert_eq!(listing(&sheets, folder), expected);
    assert!(folder.join("results/DONE").is_file());
}
