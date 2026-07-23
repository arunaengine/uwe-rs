//! Describes the inputs of every Nextflow workflow of the test workflows, the nf-core pipelines
//! and the Nextflow workflows of WorkflowHub, as a run does before its first task (decision
//! 24), and compares the counts with `tests/fixtures/inputs_test_workflows.txt`: the inputs of
//! each schema and how many are required, files and sample sheets, the errors and warnings of a
//! check of a simulated run with `-profile test` (without a profile where a WorkflowHub workflow
//! has none), and where the schema and the params that the code reads differ. Every schema must
//! be valid JSON Schema of draft 2020-12. No task runs and nothing comes from the network: the
//! driver reads local files only, and a remote file is an `IOException`, as Nextflow throws
//! when it cannot reach one; nf-core pipelines leave out their remote config files, as with
//! `NXF_OFFLINE`. Each workflow is described in a child process that starts in its folder, as a
//! run does. Run with `just inputs-test-workflows-test`; `UPDATE_FIXTURE=1` writes the fixture
//! again.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use input_schema::{Counts, OutputCounts};
use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{Driver, Launch};

mod targets;

const FIXTURE: &str = "tests/fixtures/inputs_test_workflows.txt";

/// What the counts of a workflow say when a check of its inputs finds no error.
const NO_ERRORS: &str = "tables; 0 errors";

/// A host without the network: it reads local files, and a remote file is an `IOException`.
/// What the workflow prints, also to `System.err` in config files, goes nowhere.
struct Offline;

impl Host for Offline {
    fn print(&mut self, _: &str) {}

    fn namespace_method(
        &mut self,
        namespace: &str,
        name: &str,
        _: Vec<Value>,
    ) -> Result<Option<Value>, Error> {
        match (namespace, name) {
            ("System.out" | "System.err", "println" | "print") => Ok(Some(Value::Null)),
            _ => Ok(None),
        }
    }

    fn download(&mut self, path: &PathValue) -> Result<PathBuf, Error> {
        let message = format!("Cannot read {}: the test has no network", path.uri());
        Err(Error::Thrown {
            exception: Rc::new(Exception::new("IOException", message)),
            span: Default::default(),
        })
    }
}

/// The inputs of one workflow, as the fixture records them.
fn describe(target: &Path, profile: bool, scratch: &Path) -> String {
    let outdir = scratch.join("out").to_string_lossy().into_owned();
    let launch = Launch {
        target: target.to_owned(),
        params: vec![("outdir".into(), outdir)],
        profiles: if profile {
            vec!["test".into()]
        } else {
            Vec::new()
        },
        scratch: scratch.to_owned(),
        simulated: true,
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Offline),
        ..Settings::default()
    };
    let described = Nextflow::new(&launch, settings).and_then(|mut driver| {
        let inputs = driver.inputs()?;
        let outputs = driver.outputs()?;
        Ok((inputs, outputs))
    });
    match described {
        // The counts of the outputs follow on a line of their own.
        Ok((inputs, outputs)) => {
            let inputs = inputs.map_or("none".into(), |inputs| {
                Counts::of(&inputs, true).to_string()
            });
            let outputs = outputs.map_or("none".into(), |outputs| {
                OutputCounts::of(&outputs).to_string()
            });
            format!("{inputs}\n{outputs}")
        }
        Err(error) => {
            let kind = if error.unsupported {
                "unsupported"
            } else {
                "fail"
            };
            // The child runs in the folder of the workflow, which messages leave out.
            let folder = std::env::current_dir().unwrap_or_default();
            let first = error.message.lines().next().unwrap_or_default();
            let first = first
                .replace(&format!("{}/", folder.display()), "")
                .replace(&format!("{}/", scratch.display()), "<tmp>/");
            format!("{kind}: {}", first.chars().take(200).collect::<String>())
        }
    }
}

/// Describes one workflow when the test binary is started as a child for it.
#[test]
#[ignore = "a child of the inputs test"]
fn describe_one() {
    let (Some(target), Some(outcome)) = (
        std::env::var_os("INPUTS_TARGET"),
        std::env::var_os("INPUTS_OUTCOME"),
    ) else {
        return;
    };
    let profile = std::env::var_os("INPUTS_PROFILE").is_some();
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let line = describe(Path::new(&target), profile, scratch.path());
    std::fs::write(outcome, line).expect("writes the outcome");
}

/// Describes a workflow in a child process that starts in its project folder; a workflow
/// without a `test` profile is described without one.
fn child(target: &Path, project: &Path, outcome: &Path) -> String {
    let run = |profile: bool| {
        let _ = std::fs::remove_file(outcome);
        let mut command = Command::new("timeout");
        command
            .arg("120")
            .arg(std::env::current_exe().expect("the test binary"))
            .args(["--exact", "describe_one", "--ignored", "--nocapture"])
            .args(["--test-threads", "1"])
            .env("INPUTS_TARGET", target)
            .env("INPUTS_OUTCOME", outcome)
            .env("NXF_OFFLINE", "true")
            .current_dir(project);
        if profile {
            command.env("INPUTS_PROFILE", "1");
        }
        let status = command.output().expect("runs the test binary").status;
        std::fs::read_to_string(outcome).unwrap_or_else(|_| match status.code() {
            Some(124) => "timeout".into(),
            code => format!("crash {code:?}"),
        })
    };
    let outcome = run(true);
    match outcome.ends_with("Unknown configuration profile: 'test'") {
        true => run(false),
        false => outcome,
    }
}

#[test]
#[ignore = "needs the Nextflow test workflows; run `just inputs-test-workflows-test`"]
fn workflows_describe_their_inputs() {
    let mut targets = Vec::new();
    if let Some(root) = std::env::var_os("NF_CORE_TEST_WORKFLOWS") {
        targets.extend(targets::nf_core(Path::new(&root)));
    }
    if let Some(root) = std::env::var_os("WORKFLOWHUB_TEST_WORKFLOWS") {
        targets.extend(targets::workflowhub(Path::new(&root)));
    }
    assert!(!targets.is_empty(), "no test workflows are set");
    let outcomes = tempfile::tempdir().expect("a temporary directory");
    let jobs: usize = std::env::var("IUNCTUS_TEST_WORKFLOWS_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or(4);
    let next = AtomicUsize::new(0);
    let lines = Mutex::new(vec![String::new(); targets.len()]);
    let outputs: Mutex<Vec<OutputCounts>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((name, target, project)) = targets.get(index) else {
                        break;
                    };
                    let outcome = outcomes.path().join(index.to_string());
                    let described = child(target, project, &outcome);
                    let (inputs, counts) = described.split_once('\n').unwrap_or((&described, ""));
                    if let Ok(counts) = counts.parse::<OutputCounts>() {
                        outputs.lock().expect("no child panicked").push(counts);
                    }
                    let line = format!("{name} {inputs}");
                    lines.lock().expect("no child panicked")[index] = line;
                }
            });
        }
    });
    let actual = lines.into_inner().expect("no child panicked");
    let outputs = outputs.into_inner().expect("no child panicked");
    let invalid: Vec<&OutputCounts> = outputs.iter().filter(|c| c.invalid.is_some()).collect();
    assert!(
        invalid.is_empty(),
        "schemas of outputs that are not valid: {invalid:?}"
    );
    compare(&actual, &targets, &OutputCounts::totals(&outputs));
}

/// Compares the lines with the fixture, or writes it with `UPDATE_FIXTURE=1`, with the totals
/// of the outputs as the last line. Every schema must be valid.
fn compare(actual: &[String], targets: &[targets::Target], totals: &str) {
    let counted = |prefix: &str| {
        actual
            .iter()
            .filter(|line| {
                line.split_once(' ')
                    .is_some_and(|(_, o)| o.starts_with(prefix))
            })
            .count()
    };
    let from_file = targets
        .iter()
        .zip(actual)
        .filter(|((_, _, project), line)| {
            project.join("nextflow_schema.json").is_file() && line.contains(" ok: ")
        })
        .count();
    let stopped = actual
        .iter()
        .filter(|line| line.contains(" ok: ") && !line.contains(NO_ERRORS))
        .count();
    let header = format!(
        "# The inputs of each Nextflow workflow of the test workflows (decision 24): `ok:` and the\n\
         # counts of its schema, of a check of a simulated run with -profile test, and of the\n\
         # params that its code reads and its schema does not declare or the other way round; or\n\
         # why it has no schema. {} workflows: {} schemas ({from_file} from nextflow_schema.json), \n\
         # {} invalid, {} fail to load; {stopped} runs would stop at the check.",
        actual.len(),
        counted("ok:"),
        counted("invalid"),
        actual.len() - counted("ok:") - counted("invalid"),
    );
    println!("{header}");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let text = format!("{header}\n{}\n{totals}\n", actual.join("\n"));
    if std::env::var_os("UPDATE_FIXTURE").is_some() {
        std::fs::write(&fixture, &text).expect("writes the fixture");
    }
    let recorded = std::fs::read_to_string(&fixture).unwrap_or_default();
    let recorded: Vec<&str> = recorded
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    let totals = totals.to_owned();
    let changed: Vec<&String> = (actual.iter())
        .chain([&totals])
        .filter(|line| !recorded.contains(&line.as_str()))
        .collect();
    let written = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("inputs_test_workflows.txt");
    std::fs::write(&written, &text).expect("writes the new fixture");
    assert!(
        changed.is_empty(),
        "{} workflows differ from {FIXTURE}; the new fixture is in {}:\n{}",
        changed.len(),
        written.display(),
        changed
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let invalid: Vec<&String> = actual
        .iter()
        .filter(|line| line.contains(" invalid schema: "))
        .collect();
    assert!(
        invalid.is_empty(),
        "schemas that are not valid: {invalid:?}"
    );
}
