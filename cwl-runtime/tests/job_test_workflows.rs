//! Runs the real-world CWL documents of the test workflows from `just cwl-test-workflows` with
//! their example job files, as `iunctus run <document> -inputs <job>` does with the
//! simulator, and compares the outcomes with `tests/fixtures/job_test_workflows.txt`: per
//! document and job file, `ok` and the number of tasks, or the error.
//! `fixtures/job_test_workflows.py` finds the pairs; a pair may only fail when
//! `fixtures/job_evidence.txt` has a record of it: why it cannot run, and that cwltool rejects
//! it too (see `records`). Documents that
//! `cwltool --validate` rejects, and pairs whose files the test workflows lack, are left out.
//! Run with `just cwl-job-test-workflows-test`; on a difference the test writes the new fixture
//! to the target directory and says where. `CWL_TEST_WORKFLOWS_ORDER=shuffled:<seed>` lets
//! tasks finish in a random order (see `order`).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use workflow_graph::{ExecuteError, Launch, Simulator, execute_with};

mod order;
#[path = "../../cwl-parser/tests/records/mod.rs"]
mod records;

const REJECTED: &str = include_str!("../../cwl-parser/tests/fixtures/test-workflows/rejected.txt");
/// Documents that cwltool rejects but the loader reads; they are left out like those of
/// `REJECTED`.
const DIFFERENCES: &str =
    include_str!("../../cwl-parser/tests/fixtures/test-workflows/differences.txt");
/// Why the job files that fail here cannot run, and how cwltool rejects them, from
/// `fixtures/job_evidence.py`.
const EVIDENCE: &str = include_str!("fixtures/job_evidence.txt");
/// A run that takes longer than this counts as hanging.
const LIMIT: Duration = Duration::from_secs(60);

/// The outcome of one document with one job file: `ok` and the task count, or the error.
fn outcome(root: &Path, document: &str, job: &str) -> String {
    let target = root.join(document);
    let inputs = root.join(job);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(run(target, inputs));
    });
    match receiver.recv_timeout(LIMIT) {
        Ok(Ok(count)) => format!("ok {count}"),
        Ok(Err(error)) => clean(&error, root),
        Err(_) => "timeout".into(),
    }
}

/// Runs a document with a job file through the driver that `iunctus run` uses.
fn run(target: PathBuf, inputs: PathBuf) -> Result<usize, String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let launch = Launch {
        target: target.clone(),
        inputs: Some(inputs),
        scratch: directory.path().to_path_buf(),
        assume_inputs: true,
        ..Launch::default()
    };
    let describe = |error: workflow_graph::DriverError| {
        let kind = if error.unsupported {
            "unsupported"
        } else {
            "fail"
        };
        format!("{kind}: {}", error.message)
    };
    let mut driver = cwl_runtime::driver(&launch).map_err(describe)?;
    let mut executor = Simulator::new(directory.path().join("simulator"));
    let execution = execute_with(
        &target.to_string_lossy(),
        driver.as_mut(),
        &mut executor,
        &order::run_options(),
    );
    if let Some(error) = execution.error {
        return Err(match error {
            ExecuteError::Driver(error) => describe(error),
            other => format!("fail: {other}"),
        });
    }
    execution
        .graph
        .check()
        .map_err(|error| format!("fail: the graph is not valid: {error}"))?;
    Ok(execution.graph.nodes.len())
}

/// The first line of a message, with the test workflows folder and temporary files taken out.
fn clean(message: &str, root: &Path) -> String {
    let first = message.lines().next().unwrap_or_default();
    let mut text = first
        .replace(&format!("file://{}/", root.display()), "")
        .replace(&format!("{}/", root.display()), "");
    let temporary = std::env::temp_dir().to_string_lossy().into_owned();
    while let Some(start) = text.find(&temporary) {
        let end = text[start..]
            .find([' ', '`', '"', ','])
            .map_or(text.len(), |end| start + end);
        text.replace_range(start..end, "<tmp>");
    }
    text.chars().take(240).collect()
}

#[test]
#[ignore = "needs the CWL test workflows from `just cwl-test-workflows`; run it with `just cwl-job-test-workflows-test`"]
fn documents_run_with_their_job_files_as_recorded() {
    let root =
        PathBuf::from(std::env::var("CWL_TEST_WORKFLOWS").expect("CWL_TEST_WORKFLOWS is set"));
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/job_test_workflows.txt");
    let text = std::fs::read_to_string(&fixture).unwrap_or_default();
    let header: Vec<&str> = text.lines().filter(|line| line.starts_with('#')).collect();
    let rejected = records::reasons(REJECTED);
    let differences: Vec<&str> = DIFFERENCES
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split(' ').next())
        .collect();
    // `CWL_DOCUMENT=<part of a path>` runs only the documents that contain it.
    let only = std::env::var("CWL_DOCUMENT").ok();
    let mut expected = BTreeMap::new();
    let mut actual = Vec::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let mut words = line.splitn(3, ' ');
        let (Some(document), Some(job)) = (words.next(), words.next()) else {
            continue;
        };
        let pair = format!("{document} {job}");
        expected.insert(pair.clone(), words.next().unwrap_or_default().to_owned());
        let present = root.join(document).exists() && root.join(job).exists();
        let chosen = only
            .as_ref()
            .is_none_or(|only| document.contains(only.as_str()));
        let known = rejected.contains_key(document) || differences.contains(&document);
        if present && chosen && !known {
            actual.push(format!("{pair} {}", outcome(&root, document, job)));
        }
    }
    if only.is_some() {
        println!("{}", actual.join("\n"));
        return;
    }
    let ok = actual.iter().filter(|line| line.contains(" ok ")).count();
    println!(
        "{} documents with job files, {ok} run, {} fail",
        actual.len(),
        actual.len() - ok
    );
    // A pair may only fail with a record in job_evidence.txt, and a record is only kept while
    // its pair fails.
    let evidence = records::reasons(EVIDENCE);
    let unexplained: Vec<&String> = actual
        .iter()
        .filter(|line| !line.contains(" ok "))
        .filter(|line| {
            !evidence
                .keys()
                .any(|pair| line.starts_with(&format!("{pair} ")))
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "job files fail without a record in job_evidence.txt of why they cannot run:\n{}",
        unexplained
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let running: Vec<&&str> = evidence
        .keys()
        .filter(|pair| {
            actual
                .iter()
                .any(|line| line.starts_with(&format!("{pair} ok ")))
        })
        .collect();
    assert!(
        running.is_empty(),
        "job files of job_evidence.txt run now; remove their records:\n{}",
        running
            .iter()
            .map(|pair| **pair)
            .collect::<Vec<_>>()
            .join("\n")
    );
    let changed: Vec<&String> = actual
        .iter()
        .filter(|line| {
            let mut words = line.splitn(3, ' ');
            let pair = format!(
                "{} {}",
                words.next().unwrap_or_default(),
                words.next().unwrap_or_default()
            );
            expected.get(&pair).map(String::as_str) != words.next()
        })
        .collect();
    if !changed.is_empty() {
        let written = Path::new(env!("CARGO_TARGET_TMPDIR")).join("job_test_workflows.txt");
        let mut text = header.join("\n");
        text.push('\n');
        text.push_str(&actual.join("\n"));
        text.push('\n');
        std::fs::write(&written, text).expect("writes the new fixture");
        panic!(
            "{} runs differ:\n{}\n\nThe new fixture is in {}",
            changed.len(),
            changed
                .iter()
                .map(|line| line.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            written.display()
        );
    }
}

#[test]
fn every_record_names_its_kind_and_shows_evidence() {
    let reasons = records::reasons(EVIDENCE);
    assert!(reasons.len() > 50, "{} records", reasons.len());
}
