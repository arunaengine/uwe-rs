//! Runs every workflow of the Snakemake corpus with the mock executor and compares the
//! outcome with `tests/fixtures/run_corpus.txt`: `ok` and the number of tasks, or the exit
//! code (33 for unsupported features) and the first line of the error. Each workflow runs
//! the way `tests/fixtures/run_corpus_launches.txt` says, which `launches.py` next to it
//! writes: as its CI tests it, or as Snakemake's own tests run it. Input files that no rule
//! makes are assumed to exist, since the corpus holds no large data files. Each workflow runs
//! in a child process, so that a crash or a run of more than two minutes (exit code 124)
//! stays with its workflow. Run with `just snakemake-run-corpus-test`; on a difference the
//! test prints the lines of the new fixture.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use workflow_graph::{Launch, MockTes, execute};

const LAUNCHES: &str = include_str!("fixtures/run_corpus_launches.txt");

/// How a workflow is launched: its working directory and Snakefile relative to its folder,
/// config files and values, and targets. The default runs the Snakefile of the folder in it.
#[derive(Default)]
struct Launching {
    directory: Option<String>,
    snakefile: Option<String>,
    configfiles: Vec<String>,
    config: Vec<(String, String)>,
    targets: Vec<String>,
}

impl Launching {
    /// The launch of the workflow of that name in the fixture.
    fn of(name: &str) -> Launching {
        let mut launching = Launching::default();
        let Some(line) = LAUNCHES
            .lines()
            .filter(|line| !line.starts_with('#'))
            .find(|line| line.split('\t').next() == Some(name))
        else {
            return launching;
        };
        for field in line.split('\t').skip(2) {
            let (key, value) = field.split_once('=').unwrap_or((field, ""));
            match key {
                "directory" => launching.directory = Some(value.to_owned()),
                "snakefile" => launching.snakefile = Some(value.to_owned()),
                "configfile" => launching.configfiles.push(value.to_owned()),
                "config" => {
                    let (name, value) = value.split_once('=').expect("a config value");
                    launching.config.push((name.to_owned(), value.to_owned()));
                }
                "target" => launching.targets.push(value.to_owned()),
                _ => {}
            }
        }
        launching
    }

    /// The working directory of the run.
    fn workdir(&self, folder: &Path) -> PathBuf {
        match &self.directory {
            Some(directory) => folder.join(directory),
            None => folder.to_path_buf(),
        }
    }

    /// The launch of the run; the child process runs in the working directory.
    fn launch(&self, folder: &Path, scratch: &Path) -> Launch {
        let target = match &self.snakefile {
            Some(snakefile) => folder.join(snakefile),
            None => self.workdir(folder),
        };
        Launch {
            target,
            params: self.config.clone(),
            configs: self
                .configfiles
                .iter()
                .map(|file| folder.join(file))
                .collect(),
            targets: self.targets.clone(),
            scratch: scratch.to_path_buf(),
            assume_inputs: true,
            ..Launch::default()
        }
    }
}

/// The workflow folders of the corpus: the catalog repositories and Snakemake's own tests
/// that have a Snakefile where Snakemake looks for one.
fn workflows(corpus: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for (group, folder) in [
        ("catalog", corpus.join("catalog")),
        ("tests", corpus.join("snakemake-tests/tests")),
    ] {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("an entry").path();
            if path.is_dir() && snakemake_runtime::find_snakefile(&path).is_some() {
                let name = path
                    .file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .replace(' ', "_");
                found.push((format!("{group}/{name}"), path));
            }
        }
    }
    found.sort();
    found
}

/// The outcome of one run in this process, as the fixture line records it.
fn run(folder: &Path, launching: &Launching) -> String {
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let launch = launching.launch(folder, scratch.path());
    let mut driver = match snakemake_runtime::driver(&launch) {
        Ok(driver) => driver,
        Err(error) => return format!("{} {error}", if error.unsupported { 33 } else { 1 }),
    };
    let mut executor = MockTes::new(scratch.path().join("mock"));
    let execution = execute(&folder.to_string_lossy(), driver.as_mut(), &mut executor);
    match execution.error {
        None => format!("ok {}", execution.graph.nodes.len()),
        Some(error) => format!("{} {error}", if error.unsupported() { 33 } else { 1 }),
    }
}

/// The first line of an outcome, without the parts that differ between machines.
fn clean(outcome: &str, folder: &Path) -> String {
    let first = outcome.lines().next().unwrap_or_default();
    let first = first
        .replace(&format!("{}/", folder.display()), "")
        .replace(&folder.display().to_string(), ".");
    let temporary = std::env::temp_dir().display().to_string();
    let first = first.replace(&temporary, "<tmp>");
    let mut first: String = first.chars().take(300).collect();
    while first.ends_with(' ') {
        first.pop();
    }
    first
}

/// Runs one workflow when the test binary is started as a child for it.
#[test]
#[ignore = "a child of the corpus test"]
fn run_one() {
    let (Some(name), Some(folder), Some(outcome)) = (
        std::env::var_os("SNAKEMAKE_RUN_NAME"),
        std::env::var_os("SNAKEMAKE_RUN_ONE"),
        std::env::var_os("SNAKEMAKE_RUN_OUTCOME"),
    ) else {
        return;
    };
    // SAFETY: the variable is set before any other thread of this process reads it.
    unsafe { std::env::set_var("IUNCTUS_QUIET", "1") };
    let folder = PathBuf::from(folder);
    let result = run(&folder, &Launching::of(&name.to_string_lossy()));
    std::fs::write(outcome, clean(&result, &folder)).expect("writes the outcome");
}

fn child(name: &str, folder: &Path, outcome: &Path) -> String {
    let workdir = Launching::of(name).workdir(folder);
    if !workdir.is_dir() {
        return clean(
            &format!(
                "1 the working directory {} does not exist",
                workdir.display()
            ),
            folder,
        );
    }
    let status = Command::new("timeout")
        .args([
            "120",
            &std::env::current_exe()
                .expect("the test binary")
                .to_string_lossy(),
        ])
        .args([
            "--exact",
            "run_one",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("SNAKEMAKE_RUN_NAME", name)
        .env("SNAKEMAKE_RUN_ONE", folder)
        .env("SNAKEMAKE_RUN_OUTCOME", outcome)
        .current_dir(workdir)
        .output()
        .expect("runs the test binary");
    if let Ok(text) = std::fs::read_to_string(outcome) {
        return text;
    }
    match status.status.code() {
        Some(124) => "124 timeout".into(),
        Some(code) => {
            let stderr = String::from_utf8_lossy(&status.stderr);
            let reason = stderr
                .lines()
                .find(|line| line.contains("panicked") || line.contains("overflow"))
                .unwrap_or("crash");
            clean(&format!("{code} {reason}"), folder)
        }
        None => "-1 killed by a signal".into(),
    }
}

#[test]
#[ignore = "needs the Snakemake corpus; run `just snakemake-run-corpus-test`"]
fn workflows_run_as_recorded() {
    let corpus = PathBuf::from(std::env::var("SNAKEMAKE_CORPUS").expect("SNAKEMAKE_CORPUS is set"));
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_corpus.txt");
    let expected = std::fs::read_to_string(&fixture).unwrap_or_default();
    let expected: Vec<&str> = expected
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    let workflows = workflows(&corpus);
    let outcomes = tempfile::tempdir().expect("a temporary directory");
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<String>>> = Mutex::new(vec![None; workflows.len()]);
    let threads = std::thread::available_parallelism()
        .map_or(4, |count| count.get())
        .min(4);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some((name, folder)) = workflows.get(index) else {
                        break;
                    };
                    let outcome = child(name, folder, &outcomes.path().join(index.to_string()));
                    results.lock().expect("the results")[index] = Some(format!("{name} {outcome}"));
                }
            });
        }
    });
    let actual: Vec<String> = results
        .into_inner()
        .expect("the results")
        .into_iter()
        .flatten()
        .collect();
    let full = actual
        .iter()
        .filter(|line| line.split(' ').nth(1) == Some("ok"))
        .count();
    eprintln!("{full} of {} workflows give a full graph", actual.len());
    let header = "# The outcome of each workflow of the Snakemake corpus with the mock executor: `ok` and\n# the number of tasks, or the exit code (33 for unsupported features) and the first line\n# of the error.";
    if std::env::var_os("UPDATE_FIXTURE").is_some() {
        std::fs::write(&fixture, format!("{header}\n{}\n", actual.join("\n")))
            .expect("writes the fixture");
        return;
    }
    if actual != expected {
        let changed: Vec<String> = actual
            .iter()
            .filter(|line| !expected.contains(&line.as_str()))
            .cloned()
            .collect();
        panic!(
            "{} workflows differ:\n{}\n\nThe new fixture:\n{header}\n{}",
            changed.len(),
            changed.join("\n"),
            actual.join("\n")
        );
    }
}
