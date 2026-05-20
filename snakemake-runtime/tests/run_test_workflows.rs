//! Runs every workflow of the Snakemake test workflows with the mock executor and compares the
//! outcome with `tests/fixtures/run_test_workflows.txt`: `ok` and the number of tasks, or the exit
//! code (33 for unsupported features) and the first line of the error. Each workflow runs
//! the way `tests/fixtures/run_test_workflows_launches.txt` says, which `launches.py` next to it
//! writes: as its CI tests it, or as Snakemake's own tests run it. Input files that no rule
//! makes are assumed to exist, since the test workflows hold no large data files. Each workflow runs
//! in a child process, so that a crash or a run of more than two minutes (exit code 124)
//! stays with its workflow. Run with `just snakemake-run-test-workflows-test`; on a difference the
//! test prints the lines of the new fixture. `SNAKEMAKE_TEST_WORKFLOWS_FILTER` runs only the
//! workflows whose name contains it; with `UPDATE_FIXTURE=1` it updates only their lines.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use workflow_graph::{Launch, MockTes, execute};

const LAUNCHES: &str = include_str!("fixtures/run_test_workflows_launches.txt");

/// How a workflow is launched: its working directory and Snakefile relative to its folder,
/// config files and values, and targets. The default runs the Snakefile of the folder in it.
#[derive(Default)]
struct Launching {
    directory: Option<String>,
    snakefile: Option<String>,
    configfiles: Vec<String>,
    config: Vec<(String, String)>,
    targets: Vec<String>,
    /// `--profile` and `--workflow-profile`: paths relative to the folder, or names.
    profiles: Vec<String>,
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
                "profile" | "workflow-profile" => launching.profiles.push(value.to_owned()),
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
            profiles: self
                .profiles
                .iter()
                .map(|profile| {
                    let path = folder.join(profile);
                    if path.exists() {
                        path.to_string_lossy().into_owned()
                    } else {
                        profile.clone()
                    }
                })
                .collect(),
            scratch: scratch.to_path_buf(),
            assume_inputs: true,
            ..Launch::default()
        }
    }
}

/// The workflow folders of the test workflows: the catalog repositories and Snakemake's own tests
/// that have a Snakefile where Snakemake looks for one.
fn workflows(test_workflows: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for (group, folder) in [
        ("catalog", test_workflows.join("catalog")),
        ("tests", test_workflows.join("snakemake-tests/tests")),
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
    // Files on the web, like remote modules, are downloaded once for all runs.
    if let Some(root) = std::env::var_os("SNAKEMAKE_TEST_WORKFLOWS") {
        let downloads = Path::new(&root).join("downloads");
        std::fs::create_dir_all(&downloads).expect("the folder of downloads");
        std::os::unix::fs::symlink(&downloads, scratch.path().join("downloads"))
            .expect("a link to the downloads");
    }
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
#[ignore = "a child of the test-workflows test"]
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

/// The records of the workflows that cannot run: the workflow, the reason (`invalid`,
/// `missing` or `unsupported (decision N)`) and one sentence why.
const REJECTED: &str = include_str!("fixtures/run_test_workflows_rejected.txt");

/// What real Snakemake did with the workflows it could not evaluate, from `oracle.py`.
const ORACLE: &str = include_str!("fixtures/run_test_workflows_oracle.txt");

/// The lines of a fixture without comments, split at tabs.
fn records(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| line.split('\t').collect())
}

/// The problems of the records of workflows that do not run: a failing workflow without a
/// record, a record of a workflow that runs, a reason that is not one of the three, and an
/// `invalid` or `missing` workflow that real Snakemake evaluated with the same launch.
fn check_records(outcomes: &[(String, String)]) -> Vec<String> {
    let rejected: Vec<Vec<&str>> = records(REJECTED).collect();
    let launched = |name: &str| {
        records(LAUNCHES)
            .find(|fields| fields[0] == name)
            .map_or("plain", |fields| fields[1])
            .to_owned()
    };
    // Whether the dry run of Snakemake with the launch of the workflow failed.
    let snakemake_fails = |name: &str| {
        let via = launched(name);
        records(ORACLE).any(|fields| fields[0] == name && fields[1] == via && fields[2] != "0")
    };
    let mut problems = Vec::new();
    for (name, outcome) in outcomes {
        let failing = !outcome.starts_with("ok");
        let record = rejected.iter().find(|fields| fields[0] == name);
        match (failing, record) {
            (true, None) => problems.push(format!("{name}: {outcome} (no record)")),
            (false, Some(_)) => problems.push(format!("{name}: a record, but it runs")),
            (true, Some(fields)) => {
                let reason = fields.get(1).copied().unwrap_or_default();
                let sentence = fields.get(2).copied().unwrap_or_default();
                let decision = reason
                    .strip_prefix("unsupported (decision ")
                    .and_then(|rest| rest.strip_suffix(')'))
                    .is_some_and(|number| number.parse::<u32>().is_ok());
                if !(reason == "invalid" || reason == "missing" || decision) {
                    problems.push(format!("{name}: the reason {reason:?} is none of invalid, missing, unsupported (decision N)"));
                }
                if sentence.is_empty() {
                    problems.push(format!("{name}: the record says nothing about why"));
                }
                if (reason == "invalid" || reason == "missing") && !snakemake_fails(name) {
                    problems.push(format!("{name}: {reason}, but Snakemake evaluates it (no failure in the oracle file)"));
                }
            }
            (false, None) => {}
        }
    }
    problems
}

#[test]
#[ignore = "needs the Snakemake test workflows; run `just snakemake-run-test-workflows-test`"]
fn workflows_run_as_recorded() {
    let test_workflows = PathBuf::from(
        std::env::var("SNAKEMAKE_TEST_WORKFLOWS").expect("SNAKEMAKE_TEST_WORKFLOWS is set"),
    );
    let filter = std::env::var("SNAKEMAKE_TEST_WORKFLOWS_FILTER").unwrap_or_default();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_test_workflows.txt");
    let recorded = std::fs::read_to_string(&fixture).unwrap_or_default();
    let recorded: Vec<(String, String)> = recorded
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(' '))
        .map(|(name, outcome)| (name.to_owned(), outcome.to_owned()))
        .collect();
    let workflows: Vec<(String, PathBuf)> = workflows(&test_workflows)
        .into_iter()
        .filter(|(name, _)| name.contains(&filter))
        .collect();
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
                    results.lock().expect("the results")[index] = Some(outcome);
                }
            });
        }
    });
    let actual: Vec<(String, String)> = workflows
        .iter()
        .map(|(name, _)| name.clone())
        .zip(
            results
                .into_inner()
                .expect("the results")
                .into_iter()
                .flatten(),
        )
        .collect();
    let full = actual
        .iter()
        .filter(|(_, outcome)| outcome.starts_with("ok"))
        .count();
    eprintln!("{full} of {} workflows give a full graph", actual.len());
    // The outcomes of all workflows: the new ones, and the recorded ones of those not run.
    let mut all: Vec<(String, String)> = recorded
        .iter()
        .filter(|(name, _)| !name.contains(&filter))
        .cloned()
        .chain(actual.iter().cloned())
        .collect();
    all.sort();
    let lines: Vec<String> = all
        .iter()
        .map(|(name, outcome)| format!("{name} {outcome}"))
        .collect();
    let header = "# The outcome of each workflow of the Snakemake test workflows with the mock executor: `ok` and\n# the number of tasks, or the exit code (33 for unsupported features) and the first line\n# of the error.";
    if std::env::var_os("UPDATE_FIXTURE").is_some() {
        std::fs::write(&fixture, format!("{header}\n{}\n", lines.join("\n")))
            .expect("writes the fixture");
    } else {
        let changed: Vec<String> = actual
            .iter()
            .filter(|line| !recorded.contains(line))
            .map(|(name, outcome)| format!("{name} {outcome}"))
            .collect();
        assert!(
            changed.is_empty(),
            "{} workflows differ:\n{}\n\nThe new fixture:\n{header}\n{}",
            changed.len(),
            changed.join("\n"),
            lines.join("\n")
        );
    }
    let problems = check_records(&actual);
    assert!(
        problems.is_empty(),
        "{} workflows disagree with tests/fixtures/run_test_workflows_rejected.txt:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
