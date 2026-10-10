//! Compares simulated workflow outcomes and input schemas; see the crate README for controls.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use input_schema::{Counts, OutputCounts};
use snakemake_runtime::{PythonContainers, Settings};
use workflow_graph::{Launch, Order, RunOptions, Simulator, execute_with};

const LAUNCHES: &str = include_str!("fixtures/run_test_workflows_launches.txt");

/// The outcomes of the workflows that need the Python container, when it runs.
const CONTAINERS: &str = include_str!("fixtures/run_test_workflows_containers.txt");

/// What the error of a workflow that needs the Python container says without it.
const NO_RUNTIME: &str = "(decision 19), but this run has no container runtime";

/// Whether the workflows run with the Python container: `IUNCTUS_CONTAINER_TESTS=1`.
fn containers() -> bool {
    std::env::var_os("IUNCTUS_CONTAINER_TESTS").is_some()
}

/// The settings of a run: the Python container, when the test runs with it. What uv
/// downloads and the environments it makes are kept next to the test workflows.
fn settings() -> Settings {
    if !containers() {
        return Settings::default();
    }
    let root =
        std::env::var_os("SNAKEMAKE_TEST_WORKFLOWS").map_or_else(std::env::temp_dir, PathBuf::from);
    Settings {
        python: Some(PythonContainers {
            engine: std::env::var("IUNCTUS_ENGINE").unwrap_or_else(|_| "docker".into()),
            image: None,
            cache: root.join("python-cache"),
            show_output: false,
        }),
        ..Settings::default()
    }
}

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
            simulated: true,
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

/// The seed of the shuffled order of finished tasks: `SNAKEMAKE_TEST_WORKFLOWS_SEED`, else 1.
fn shuffle_seed() -> u64 {
    std::env::var("SNAKEMAKE_TEST_WORKFLOWS_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or(1)
}

/// The outcome of one run in this process, as the fixture line records it, and the counts of
/// the inputs and the outputs of the workflow, which the run describes first, as the binary
/// does.
fn run(folder: &Path, launching: &Launching, scratch: &Path) -> (String, String, String, String) {
    // Files on the web, like remote modules, are downloaded once for all runs.
    if let Some(root) = std::env::var_os("SNAKEMAKE_TEST_WORKFLOWS") {
        let downloads = Path::new(&root).join("downloads");
        std::fs::create_dir_all(&downloads).expect("the folder of downloads");
        std::os::unix::fs::symlink(&downloads, scratch.join("downloads"))
            .expect("a link to the downloads");
    }
    let launch = launching.launch(folder, scratch);
    let mut driver = match snakemake_runtime::driver_with(&launch, settings()) {
        Ok(driver) => driver,
        Err(error) => {
            let outcome = format!("{} {error}", if error.unsupported { 33 } else { 1 });
            return (outcome.clone(), outcome.clone(), outcome, String::new());
        }
    };
    let inputs = match driver.inputs() {
        Ok(Some(inputs)) => Counts::of(&inputs, true).to_string(),
        Ok(None) => "none: the workflow runs a command while it is loaded".into(),
        Err(error) => format!("{} {error}", if error.unsupported { 33 } else { 1 }),
    };
    let outputs = match driver.outputs() {
        Ok(Some(outputs)) => OutputCounts::of(&outputs).to_string(),
        Ok(None) => "none".into(),
        Err(error) => format!("{} {error}", if error.unsupported { 33 } else { 1 }),
    };
    let mut executor = Simulator::new(scratch.join("simulator"));
    // Up to eight tasks finish in a shuffled order, as tasks of a real run may.
    let options = RunOptions {
        max_pending: 8,
        order: Order::Shuffled {
            seed: shuffle_seed(),
        },
    };
    let execution = execute_with(
        &folder.to_string_lossy(),
        driver.as_mut(),
        &mut executor,
        &options,
    );
    let outcome = match execution.error {
        None => format!("ok {}", execution.graph.nodes.len()),
        Some(error) => format!("{} {error}", if error.unsupported() { 33 } else { 1 }),
    };
    let mut jobs = BTreeMap::<String, usize>::new();
    for node in &execution.graph.nodes {
        *jobs.entry(node.task.step.clone()).or_default() += 1;
    }
    jobs.insert("total".into(), execution.graph.nodes.len());
    (
        outcome,
        inputs,
        outputs,
        serde_json::to_string(&jobs).unwrap(),
    )
}

/// The file next to the outcome of a child that holds the counts of the inputs.
fn inputs_file(outcome: &Path) -> PathBuf {
    outcome.with_extension("inputs")
}

/// The file next to the outcome of a child that holds the counts of the outputs.
fn outputs_file(outcome: &Path) -> PathBuf {
    outcome.with_extension("outputs")
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
    // The parent removes the scratch folder, also after a timeout ended this process.
    let scratch = std::env::var_os("SNAKEMAKE_RUN_SCRATCH").map(PathBuf::from);
    let own = scratch
        .is_none()
        .then(|| tempfile::tempdir().expect("a temporary directory"));
    let scratch = scratch.unwrap_or_else(|| own.as_ref().unwrap().path().to_path_buf());
    let (result, inputs, outputs, jobs) =
        run(&folder, &Launching::of(&name.to_string_lossy()), &scratch);
    let outcome = PathBuf::from(outcome);
    std::fs::write(outcome.with_extension("jobs"), jobs).expect("writes the rule counts");
    std::fs::write(outputs_file(&outcome), outputs).expect("writes the outputs");
    std::fs::write(inputs_file(&outcome), clean(&inputs, &folder)).expect("writes the inputs");
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
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let limit = if containers() { "900" } else { "120" };
    let status = Command::new("timeout")
        .args([
            limit,
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
        .env("SNAKEMAKE_RUN_SCRATCH", scratch.path())
        // Some workflows require `envvars: "LC_CTYPE"`; a fixed value keeps records stable.
        .env("LC_CTYPE", "C.UTF-8")
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
/// `missing`, `runtime`, `timeout` or `unsupported (decision N)`) and one sentence why.
const REJECTED: &str = include_str!("fixtures/run_test_workflows_rejected.txt");

/// What real Snakemake did with the workflows it could not evaluate, from `oracle.py`.
const ORACLE: &str = include_str!("fixtures/run_test_workflows_oracle.txt");

/// The lines of a fixture without comments, split at tabs.
fn records(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| line.split('\t').collect())
}

/// Checks missing or unexpected rejection records, reasons, and native evaluation outcomes.
/// Workflows needing unavailable Python containers belong in run_test_workflows_containers.txt.
fn check_records(outcomes: &[(String, String)]) -> Vec<String> {
    let listed = |name: &str| {
        CONTAINERS.lines().any(|line| {
            line.split_once(' ')
                .is_some_and(|(listed, _)| listed == name)
        })
    };
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
            (true, None) if outcome.contains(NO_RUNTIME) => {
                if !listed(name) {
                    problems.push(format!("{name}: needs the Python container, but run_test_workflows_containers.txt does not list it"));
                }
            }
            (true, None) => problems.push(format!("{name}: {outcome} (no record)")),
            (false, Some(_)) => problems.push(format!("{name}: a record, but it runs")),
            (true, Some(fields)) => {
                let reason = fields.get(1).copied().unwrap_or_default();
                let sentence = fields.get(2).copied().unwrap_or_default();
                let decision = reason
                    .strip_prefix("unsupported (decision ")
                    .and_then(|rest| rest.strip_suffix(')'))
                    .is_some_and(|number| number.parse::<u32>().is_ok());
                if !(matches!(reason, "invalid" | "missing" | "runtime" | "timeout") || decision) {
                    problems.push(format!("{name}: the reason {reason:?} is none of invalid, missing, runtime, timeout, unsupported (decision N)"));
                }
                if reason == "timeout" && !outcome.starts_with("124 ") {
                    problems.push(format!(
                        "{name}: the record says timeout, but the outcome is {outcome}"
                    ));
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
fn matches_recorded_rejections() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_test_workflows.txt");
    let text = std::fs::read_to_string(path).expect("the recorded outcomes");
    let outcomes: Vec<_> = text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(' '))
        .map(|(name, outcome)| (name.to_owned(), outcome.to_owned()))
        .collect();
    let problems = check_records(&outcomes);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
#[ignore = "needs the Snakemake test workflows; run `just snakemake-run-test-workflows-test`"]
fn matches_recorded_workflows() {
    let test_workflows = PathBuf::from(
        std::env::var("SNAKEMAKE_TEST_WORKFLOWS").expect("SNAKEMAKE_TEST_WORKFLOWS is set"),
    );
    let filter = std::env::var("SNAKEMAKE_TEST_WORKFLOWS_FILTER").unwrap_or_default();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let read = |fixture: &Path| -> Vec<(String, String)> {
        std::fs::read_to_string(fixture)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once(' '))
            .map(|(name, outcome)| (name.to_owned(), outcome.to_owned()))
            .collect()
    };
    // With the Python container, only the workflows that need it run, into their own file.
    let without = read(&fixtures.join("run_test_workflows.txt"));
    let fixture = fixtures.join(if containers() {
        "run_test_workflows_containers.txt"
    } else {
        "run_test_workflows.txt"
    });
    let recorded = read(&fixture);
    let needs_container = |name: &str| {
        without
            .iter()
            .any(|(listed, outcome)| listed == name && outcome.contains(NO_RUNTIME))
    };
    let workflows: Vec<(String, PathBuf)> = workflows(&test_workflows)
        .into_iter()
        .filter(|(name, _)| name.contains(&filter))
        .filter(|(name, _)| !containers() || needs_container(name))
        .collect();
    let outcomes = tempfile::tempdir().expect("a temporary directory");
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<String>>> = Mutex::new(vec![None; workflows.len()]);
    let inputs: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let outputs: Mutex<Vec<OutputCounts>> = Mutex::new(Vec::new());
    let jobs: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
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
                    let file = outcomes.path().join(index.to_string());
                    let outcome = child(name, folder, &file);
                    if outcome.starts_with("ok") {
                        let counts = std::fs::read_to_string(file.with_extension("jobs"))
                            .expect("the planned rule counts");
                        jobs.lock().expect("the jobs").insert(name.clone(), counts);
                    }
                    let counts = std::fs::read_to_string(inputs_file(&file))
                        .unwrap_or_else(|_| outcome.clone());
                    inputs
                        .lock()
                        .expect("the inputs")
                        .push((name.clone(), counts));
                    let described = std::fs::read_to_string(outputs_file(&file));
                    if let Some(counts) = described.ok().and_then(|text| text.parse().ok()) {
                        outputs.lock().expect("the outputs").push(counts);
                    }
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
    let jobs = jobs.into_inner().expect("the jobs");
    if let Ok(path) = std::env::var("SNAKEMAKE_JOB_RECORD") {
        std::fs::write(path, serde_json::to_string_pretty(&jobs).unwrap()).unwrap();
    }
    if !containers() {
        let mut inputs = inputs.into_inner().expect("the inputs");
        let outputs = outputs.into_inner().expect("the outputs");
        let invalid: Vec<&OutputCounts> = outputs.iter().filter(|c| c.invalid.is_some()).collect();
        assert!(
            invalid.is_empty(),
            "schemas of outputs that are not valid: {invalid:?}"
        );
        // The totals of the outputs of all workflows, which a filtered run does not have.
        if filter.is_empty() {
            inputs.push(("(outputs)".into(), OutputCounts::totals(&outputs)));
        }
        inputs.sort();
        compare_inputs(
            &fixtures.join("run_test_workflows_inputs.txt"),
            &inputs,
            &filter,
        );
    }
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
    let header = if containers() {
        "# The outcome of each workflow of the Snakemake test workflows that imports a Python package\n# without a stand-in while it is planned, with the Python container of decision 19 and the\n# simulator: `ok` and the number of tasks, or the exit code and the first line of the\n# error. Without a container runtime, these workflows stop with an error that says so."
    } else {
        "# The outcome of each workflow of the Snakemake test workflows with the simulator: `ok` and the\n# number of tasks, or the exit code (33 for unsupported features) and the first line\n# of the error."
    };
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
    if !containers() {
        compare_jobs(&fixtures, &jobs);
    }
}

/// Compares real dry-run jobs with simulated tasks, retaining only exact documented differences.
fn compare_jobs(fixtures: &Path, jobs: &BTreeMap<String, String>) {
    let oracle = std::fs::read_to_string(fixtures.join("run_test_workflows_jobs.txt"))
        .expect("the real Snakemake rule counts");
    let expected = std::fs::read_to_string(fixtures.join("job_differences.txt"))
        .expect("the documented job differences");
    let documented: Vec<_> = records(&expected).collect();
    let mut problems = Vec::new();
    let mut matched = 0;
    let mut compared = 0;
    let mut unavailable = 0;
    let mut errors = 0;
    for (name, planned) in jobs {
        let Some(fields) = records(&oracle).find(|fields| fields[0] == name) else {
            problems.push(format!("{name}: no real Snakemake record"));
            continue;
        };
        if fields[3] == "null" {
            unavailable += 1;
            eprintln!(
                "{name}: oracle unavailable: {}",
                fields.get(4).unwrap_or(&"")
            );
            continue;
        }
        errors += usize::from(fields[2] != "0");
        compared += 1;
        let actual: BTreeMap<String, usize> = serde_json::from_str(planned).unwrap();
        let real: BTreeMap<String, usize> = serde_json::from_str(fields[3]).unwrap();
        let record = documented.iter().find(|fields| fields[0] == name);
        if actual == real {
            matched += 1;
            if record.is_some() {
                problems.push(format!("{name}: obsolete documented difference"));
            }
            continue;
        }
        if let Some(record) = record {
            let allowed: BTreeMap<String, usize> = serde_json::from_str(record[1]).unwrap();
            let reference: BTreeMap<String, usize> = serde_json::from_str(record[2]).unwrap();
            if actual == allowed
                && real == reference
                && record.get(3).is_some_and(|reason| !reason.is_empty())
            {
                continue;
            }
        }
        let mut rules: Vec<_> = actual.keys().chain(real.keys()).collect();
        rules.sort();
        rules.dedup();
        let differences: Vec<_> = rules
            .into_iter()
            .filter(|rule| actual.get(*rule) != real.get(*rule))
            .map(|rule| {
                format!(
                    "{rule}: planned {}, Snakemake {}",
                    actual.get(rule).unwrap_or(&0),
                    real.get(rule).unwrap_or(&0)
                )
            })
            .collect();
        problems.push(format!("{name}: {}", differences.join(", ")));
    }
    eprintln!(
        "{matched} exact matches of {compared} compared workflows; {unavailable} without oracle counts; {errors} oracle errors after job statistics"
    );
    assert!(
        problems.is_empty(),
        "job count differences:\n{}",
        problems.join("\n")
    );
}

#[test]
#[should_panic(expected = "first: planned 1, Snakemake 0")]
fn rejects_rule_differences() {
    let fixtures = tempfile::tempdir().unwrap();
    std::fs::write(
        fixtures.path().join("run_test_workflows_jobs.txt"),
        "example\tplain\t1\t{\"second\":1,\"total\":1}\tformatting failed\n",
    )
    .unwrap();
    std::fs::write(fixtures.path().join("job_differences.txt"), "").unwrap();
    compare_jobs(
        fixtures.path(),
        &BTreeMap::from([("example".into(), "{\"first\":1,\"total\":1}".into())]),
    );
}

#[test]
#[should_panic(expected = "second: planned 0, Snakemake 2")]
fn rejects_changed_differences() {
    let fixtures = tempfile::tempdir().unwrap();
    std::fs::write(
        fixtures.path().join("run_test_workflows_jobs.txt"),
        "example\tplain\t0\t{\"second\":2,\"total\":2}\n",
    )
    .unwrap();
    std::fs::write(
        fixtures.path().join("job_differences.txt"),
        "example\t{\"first\":1,\"total\":1}\t{\"second\":1,\"total\":1}\tGrouped tasks.\n",
    )
    .unwrap();
    compare_jobs(
        fixtures.path(),
        &BTreeMap::from([("example".into(), "{\"first\":1,\"total\":1}".into())]),
    );
}

/// Compares the counts of the inputs of the workflows that ran with the fixture, or writes it
/// with `UPDATE_FIXTURE=1`, keeping the lines of the workflows that did not run. Every schema
/// must be valid.
fn compare_inputs(fixture: &Path, inputs: &[(String, String)], filter: &str) {
    let invalid: Vec<String> = (inputs.iter())
        .filter(|(_, counts)| counts.starts_with("invalid schema"))
        .map(|(name, counts)| format!("{name} {counts}"))
        .collect();
    assert!(
        invalid.is_empty(),
        "schemas that are not valid:\n{}",
        invalid.join("\n")
    );
    let recorded: Vec<(String, String)> = std::fs::read_to_string(fixture)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(' '))
        .map(|(name, counts)| (name.to_owned(), counts.to_owned()))
        .collect();
    if std::env::var_os("UPDATE_FIXTURE").is_some() {
        let mut all: Vec<(String, String)> = (recorded.iter())
            .filter(|(name, _)| !name.contains(filter))
            .cloned()
            .chain(inputs.iter().cloned())
            .collect();
        all.sort();
        let described = all
            .iter()
            .filter(|(_, counts)| counts.starts_with("ok:"))
            .count();
        let lines: Vec<String> = (all.iter())
            .map(|(name, counts)| format!("{name} {counts}"))
            .collect();
        let header = format!(
            "# The inputs of each workflow of the Snakemake test workflows (decision 24), as a run\n\
             # describes them before its first task: `ok:` and the counts of its schema, of a check\n\
             # of the simulated run, and of the config keys that its code reads and its schema does\n\
             # not declare or the other way round; or the error that stops its evaluation. The line\n\
             # `(outputs)` holds the totals of the schemas of their outputs.\n\
             # {} workflows, {described} with a schema.",
            all.iter().filter(|(name, _)| name != "(outputs)").count()
        );
        std::fs::write(fixture, format!("{header}\n{}\n", lines.join("\n")))
            .expect("writes the fixture of the inputs");
        return;
    }
    let changed: Vec<String> = (inputs.iter())
        .filter(|line| !recorded.contains(line))
        .map(|(name, counts)| format!("{name} {counts}"))
        .collect();
    assert!(
        changed.is_empty(),
        "the inputs of {} workflows differ from {}:\n{}",
        changed.len(),
        fixture.display(),
        changed.join("\n")
    );
}
