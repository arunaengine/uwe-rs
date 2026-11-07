//! nf-bids against the plugin 0.3.0 with Nextflow 26.04.6: the probe of `fixtures/nf_bids` on
//! the recorded datasets, options and bad configs, and the operator probe.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_runtime::driver::{Nextflow, Settings};
use nextflow_runtime::{Loader, Options, Run};
use workflow_graph::{Attempt, Engine, Executor, Launch, Order, Outcome, RunOptions, Running};

struct Console {
    lines: Vec<String>,
    launch: String,
}

impl Host for Console {
    fn print(&mut self, text: &str) {
        self.lines.extend(text.lines().map(str::to_owned));
    }

    fn launch_dir(&self) -> String {
        self.launch.clone()
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_bids")
}

fn read(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("reads");
    if path.extension().is_some_and(|extension| extension == "gz") {
        let mut text = String::new();
        let mut reader = flate2::read::GzDecoder::new(bytes.as_slice());
        reader.read_to_string(&mut text).expect("unpacks");
        return text;
    }
    String::from_utf8(bytes).expect("text")
}

/// Creates a dataset of the recording in `<launch>/ds/<name>`: its files are empty, but for
/// those of `ds-mini` that the fixtures keep and the files that `contents` gives.
fn dataset(launch: &Path, name: &str, contents: &[(&str, String)]) {
    let root = launch.join("ds").join(name);
    let list = read(&fixtures().join(format!("datasets/{name}.files.tsv")));
    for line in list.lines() {
        let relative = line.split('\t').next().unwrap_or_default();
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("creates");
        let kept = match relative {
            ".bidsignore" | "participants.tsv" => {
                format!("{name}.{}", relative.trim_start_matches('.'))
            }
            "sub-01/anat/sub-01_T1w.json" if name == "ds-mini" => "ds-mini.sub-01_T1w.json".into(),
            _ => String::new(),
        };
        let text = match contents.iter().find(|(file, _)| *file == relative) {
            Some((_, text)) => text.clone(),
            None if !kept.is_empty() && fixtures().join("datasets").join(&kept).exists() => {
                read(&fixtures().join("datasets").join(&kept))
            }
            None => String::new(),
        };
        std::fs::write(path, text).expect("writes");
    }
}

/// Runs a script with nf-bids turned on and `params`, in a launch folder with the configs.
fn run(launch: &Path, script: &str, params: &[(&str, &str)]) -> (Vec<String>, Result<(), Error>) {
    let copy = |from: &Path, to: &Path| {
        std::fs::create_dir_all(to.parent().expect("a folder")).expect("creates");
        std::fs::copy(from, to).expect("copies");
    };
    // The recording gave the bad configs as `bad/<case>.yaml`.
    for (folder, target) in [("configs", "configs"), ("configs/bad", "bad")] {
        for entry in std::fs::read_dir(fixtures().join(folder)).expect("lists") {
            let path = entry.expect("an entry").path();
            if path.is_file() {
                let name = path.file_name().expect("a name");
                copy(&path, &launch.join(target).join(name));
            }
        }
    }
    copy(&fixtures().join(script), &launch.join("main.nf"));
    let mut console = Console {
        lines: Vec::new(),
        launch: launch.to_string_lossy().into_owned(),
    };
    let file = launch.join("nextflow.config");
    std::fs::write(&file, "plugins { id 'nf-bids@0.3.0' }\n").expect("writes");
    let params = params
        .iter()
        .map(|(name, value)| (Value::string(*name), Value::string(*value)));
    let settings = nextflow_config::Options {
        profiles: None,
        params: Value::map(params.collect::<Vec<_>>()),
        project_dir: console.launch.clone(),
        launch_dir: console.launch.clone(),
        work_dir: launch.join("work").to_string_lossy().into_owned(),
    };
    let config = nextflow_config::load(&[file], &settings, &mut console).expect("loads");
    let options = Options {
        config,
        project_dir: console.launch.clone(),
        script_file: launch.join("main.nf").to_string_lossy().into_owned(),
        ..Options::default()
    };
    let module = Loader::default()
        .load(&launch.join("main.nf"))
        .expect("loads");
    let result = Run::start(&module, &options, &mut console).and_then(|(mut run, requests)| {
        assert!(requests.is_empty(), "the probes run no tasks");
        run.finish(&mut console)
    });
    (console.lines, result)
}

/// The lines of a recording without Nextflow's own and the probe's option line.
fn recorded(text: &str, launch: &Path) -> Vec<String> {
    let skipped = [
        "[PIPELINE]",
        "[WORKDIR]",
        "[SUCCESS]",
        "[FAILED]",
        "[exit code",
        "opts:",
    ];
    (text.lines())
        .filter(|line| !line.is_empty() && !skipped.iter().any(|start| line.starts_with(start)))
        .map(|line| line.replace("<probe-dir>", &launch.to_string_lossy()))
        .collect()
}

fn check(name: &str, config: &str, options: &str, recording: &str, contents: &[(&str, String)]) {
    let launch = tempfile::tempdir().expect("a folder");
    dataset(launch.path(), name, contents);
    let dataset = format!("ds/{name}");
    let mut params = vec![("dataset", dataset.as_str()), ("config", config)];
    if !options.is_empty() {
        params.push(("opts", options));
    }
    let (lines, result) = run(launch.path(), "frombids-probe.nf", &params);
    result.unwrap_or_else(|error| panic!("{recording}: {error}"));
    let expected = recorded(&read(&fixtures().join(recording)), launch.path());
    assert_eq!(lines, expected, "{recording}");
}

#[test]
fn groups_recorded_datasets() {
    for name in ["ds-dwi", "ds-dwi2", "ds-dwi3"] {
        check(
            name,
            "configs/config_dwi.yaml",
            "",
            &format!("fromBIDS/{name}.txt"),
            &[],
        );
    }
    check(
        "ds-dwi4",
        "configs/config_dwi.yaml",
        "",
        "fromBIDS/ds-dwi4.txt.gz",
        &[],
    );
    check(
        "ds-mrs_fmrs",
        "configs/config_mrs.yaml",
        "",
        "fromBIDS/ds-mrs_fmrs.txt",
        &[],
    );
    check(
        "ds-mtsat",
        "configs/config_mtsat.yaml",
        "",
        "fromBIDS/ds-mtsat.txt",
        &[],
    );
    let multiruns = "configs/config_t1w_fmap_multiruns.yaml";
    check(
        "ds-t1w-fmap-multiruns",
        multiruns,
        "",
        "fromBIDS/ds-t1w-fmap-multiruns.txt",
        &[],
    );
}

#[test]
fn groups_versatile_sets() {
    let names = [
        "ds-neuromod",
        "ds-t1_dwi_b0_norevb0",
        "ds-t1_dwi_nob0_multiple_revb0",
        "ds-t1_dwi_nob0_norevb0",
        "ds-t1_dwi_nob0_revb0",
        "ds-t1_dwi_nob0_revb0_nosession",
        "ds-t1_dwi_nob0_revb0_old_intendedfor",
        "ds-t1_dwi_revdwi_nob0_norevb0",
        "ds-t1_dwi_two_subjects_diff_dir_nob0_norevb0",
        "ds-t1_multiple_dwi_nob0_multiple_revb0",
    ];
    for name in names {
        let config = "configs/config_dwi_versatile.yaml";
        check(name, config, "", &format!("fromBIDS/{name}.txt"), &[]);
    }
}

#[test]
fn follows_options() {
    let config = "configs/cfg-mini.yaml";
    check("ds-mini", config, "{}", "options/ds-mini-1.txt", &[]);
    check(
        "ds-mini",
        config,
        "{\"flatten_output\": false}",
        "options/ds-mini-2.txt",
        &[],
    );
    check(
        "ds-mini",
        config,
        "{\"use_bidsignore\": false}",
        "options/ds-mini-4.txt",
        &[],
    );
    check(
        "ds-mini",
        config,
        "{\"use_default_ignores\": false}",
        "options/ds-mini-5.txt",
        &[],
    );
    // The recording keeps the DWI sidecar only as the item shows it.
    let recording = read(&fixtures().join("options/ds-mini-3.txt"));
    let start = recording.find("\"dwi\":").expect("a dwi entry");
    let open = start + recording[start..].find("\"json\":{").expect("a sidecar") + 7;
    let mut depth = 0;
    let end = (recording[open..].char_indices())
        .find(|(_, c)| {
            depth += match c {
                '{' => 1,
                '}' => -1,
                _ => 0,
            };
            depth == 0
        })
        .map(|(at, _)| open + at + 1)
        .expect("the end of the sidecar");
    let sidecar = vec![(
        "sub-01/dwi/sub-01_dwi.json",
        recording[open..end].to_owned(),
    )];
    let options = "{\"unpack_json_sidecar\": true}";
    check(
        "ds-mini",
        config,
        options,
        "options/ds-mini-3.txt",
        &sidecar,
    );
}

#[test]
fn reports_config_errors() {
    for case in ["loopstr", "meta", "noset", "order", "required", "syntax"] {
        let launch = tempfile::tempdir().expect("a folder");
        dataset(launch.path(), "ds-dwi", &[]);
        let config = format!("bad/{case}.yaml");
        let params = [("dataset", "ds/ds-dwi"), ("config", config.as_str())];
        let (_, result) = run(launch.path(), "frombids-probe.nf", &params);
        let recording = read(&fixtures().join(format!("errors/config-{case}.txt")));
        let message = recording.split("ERROR ~ ").nth(1).expect("an error");
        let message = message
            .split("\n\n -- Check")
            .next()
            .expect("a message")
            .trim_end();
        let error = result.expect_err("the config fails").to_string();
        if case == "syntax" {
            assert!(
                error.contains("Failed to load BIDS configuration from bad/syntax.yaml: "),
                "{error}"
            );
        } else {
            assert!(
                error.contains(message),
                "{case}: {error}\nwanted: {message}"
            );
        }
    }
    let launch = tempfile::tempdir().expect("a folder");
    let params = [
        ("dataset", "ds/nope"),
        ("config", "configs/config_dwi.yaml"),
    ];
    let error = run(launch.path(), "frombids-probe.nf", &params)
        .1
        .expect_err("fails")
        .to_string();
    let missing = format!(
        "BIDS directory does not exist: {}/ds/nope",
        launch.path().display()
    );
    assert!(error.contains(&missing), "{error}");
}

#[test]
fn applies_key_operators() {
    let launch = tempfile::tempdir().expect("a folder");
    let (mut lines, result) = run(launch.path(), "operators.nf", &[]);
    result.expect("the operators run");
    let mut expected = recorded(&read(&fixtures().join("operators.out")), launch.path());
    lines.sort();
    expected.sort();
    assert_eq!(lines, expected);
}

/// Runs the tasks of `streaming.nf`: `make 3` ends only once `use a` started, or after a
/// generous cap, so that the order tells whether the full group went on while `make 3` ran.
struct Gate {
    order: Arc<Mutex<Vec<String>>>,
    open: mpsc::Sender<()>,
    gate: Option<mpsc::Receiver<()>>,
}

impl Executor for Gate {
    fn run(&mut self, attempt: Attempt) -> Running {
        let script = attempt.task.script.clone().unwrap_or_default();
        let script = script.lines().last().unwrap_or_default().trim().to_owned();
        let order = self.order.clone();
        order.lock().expect("the order").push(script.clone());
        let outcome = Outcome {
            id: attempt.task.id,
            ..Outcome::default()
        };
        if script == "use a" {
            let _ = self.open.send(());
        }
        let gate = (script == "make 3").then(|| self.gate.take()).flatten();
        match gate {
            Some(gate) => Running::new(async move {
                let waited = move || gate.recv_timeout(Duration::from_secs(60));
                let _ = tokio::task::spawn_blocking(waited).await;
                order.lock().expect("the order").push("make 3 ended".into());
                Ok(outcome)
            }),
            None => Running::ended(Ok(outcome)),
        }
    }
}

#[test]
fn sends_full_groups() {
    let launch = tempfile::tempdir().expect("a folder");
    let folder = launch.path();
    std::fs::copy(fixtures().join("streaming.nf"), folder.join("main.nf")).expect("copies");
    let config = "plugins { id 'nf-bids@0.3.0' }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes");
    let run = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let console = Console {
        lines: Vec::new(),
        launch: folder.to_string_lossy().into_owned(),
    };
    let settings = Settings {
        host: Box::new(console),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&run, settings).expect("a driver");
    let (open, gate) = mpsc::channel();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut executor = Gate {
        order: order.clone(),
        open,
        gate: Some(gate),
    };
    let options = RunOptions {
        max_pending: 8,
        order: Order::Finished,
    };
    let execution = Engine::new(&mut executor)
        .options(options)
        .run("main.nf", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let order = order.lock().expect("the order").clone();
    let at = |name: &str| order.iter().position(|step| step == name);
    assert!(at("use a") < at("make 3 ended"), "{order:?}");
    assert!(at("use b").is_some(), "{order:?}");
}
