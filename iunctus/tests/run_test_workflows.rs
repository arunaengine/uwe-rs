//! Runs every nf-core pipeline of the test workflows with `-profile test` and the mock TES
//! executor, and compares the outcome with `tests/fixtures/run_test_workflows.txt`: `ok`, or the
//! exit code and the first line of the error. The runs are stub runs, as pipelines then do not
//! read the content of the outputs that the mock makes up, and up to 8 tasks finish in a seeded
//! shuffled order, so that the Nextflow driver cannot depend on the order of outcomes. The
//! execution graph of each run that succeeds must be valid. A pipeline may only fail with a
//! record in `tests/fixtures/run_evidence.txt` of why it cannot run, and a record is only kept
//! while its pipeline fails. Run with `just run-test-workflows-test`; on a difference the test
//! prints the lines of the new fixture.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use fancy_regex::Regex;
use workflow_graph::ExecutionGraph;

/// Why each pipeline that fails cannot run, with the output of Nextflow, recorded by
/// `fixtures/run_evidence.py`.
const EVIDENCE: &str = include_str!("fixtures/run_evidence.txt");

/// The reason of each pipeline that a record names. A record is a block of lines between
/// empty lines: the pipelines, then `= <kind>: <why>` with one plain sentence, then the
/// evidence that Nextflow fails the same way: `$` the command, and `>` the lines of its output.
/// The kind is `invalid` (the pipeline itself is broken), `missing` (it needs something that is
/// not public or not in its repository), or `unsupported (decision N)` (iunctus leaves the
/// feature out on purpose, as decision N of docs/decisions.md says). Panics on a record without
/// a valid reason or without evidence, so that the record file stays complete.
fn reasons(text: &str) -> BTreeMap<&str, &str> {
    let mut reasons = BTreeMap::new();
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let pipelines: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|line| !line.starts_with(['=', '$', '>']))
            .collect();
        if pipelines.is_empty() {
            continue;
        }
        let reason = lines.iter().find_map(|line| line.strip_prefix("= "));
        let Some(reason) = reason.filter(|reason| valid(reason)) else {
            panic!(
                "the record of {pipelines:?} needs a line `= invalid: <why>`, \
                 `= missing: <why>` or `= unsupported (decision N): <why>`"
            );
        };
        assert!(
            lines.iter().any(|line| line.starts_with('>')),
            "the record of {pipelines:?} shows no output of Nextflow"
        );
        for pipeline in pipelines {
            reasons.insert(pipeline, reason);
        }
    }
    reasons
}

/// Whether a reason names one of the three kinds and says why.
fn valid(reason: &str) -> bool {
    let Some((kind, why)) = reason.split_once(": ") else {
        return false;
    };
    let decision = kind
        .strip_prefix("unsupported (decision ")
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(|number| number.parse::<u32>().is_ok());
    (matches!(kind, "invalid" | "missing") || decision) && !why.trim().is_empty()
}

/// The outcome of one run: `ok`, or the exit code and the first error line without the
/// parts that differ between machines and runs. A run that takes more than ten minutes is
/// stopped with exit code 124. A run that succeeds
/// with an invalid graph is `graph` and the problem.
fn outcome(test_workflows: &Path, pipeline: &str, cache: &Path) -> String {
    let graph = cache.join("graph.json");
    let _ = std::fs::remove_file(&graph);
    let output = Command::new("timeout")
        .args(["600", env!("CARGO_BIN_EXE_iunctus")])
        .args(["run", &test_workflows.join(pipeline).to_string_lossy()])
        .args(["-profile", "test", "-stub-run"])
        .args(["-max-pending", "8", "-order", "shuffled:1"])
        .args(["-graph", &graph.to_string_lossy()])
        .args(["--outdir", &cache.join("out").to_string_lossy()])
        .env("XDG_CACHE_HOME", cache)
        .current_dir(cache)
        .output()
        .expect("runs iunctus");
    let code = output.status.code().unwrap_or(-1);
    if code == 0 {
        let text = std::fs::read_to_string(&graph).unwrap_or_default();
        let checked = serde_json::from_str::<ExecutionGraph>(&text)
            .map_err(|error| error.to_string())
            .and_then(|graph| graph.check());
        return match checked {
            Ok(()) => "ok".into(),
            Err(problem) => format!("graph {problem}"),
        };
    }
    let error = String::from_utf8_lossy(&output.stderr);
    let first = error.lines().next().unwrap_or_default();
    let prefix = format!("{}/", test_workflows.join(pipeline).display());
    let first = first
        .replace(&prefix, "")
        .replace(&cache.display().to_string(), "<cache>");
    let run = Regex::new(r"[0-9a-f]{9,}-[0-9a-f]+").expect("a pattern");
    let time = Regex::new(r"\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}").expect("a pattern");
    let address = Regex::new(r"@[0-9a-f]{6,}").expect("a pattern");
    let first = run.replace_all(&first, "<run>");
    let first = address.replace_all(&first, "@<address>");
    let first = time.replace_all(&first, "<time>");
    if first.is_empty() {
        return code.to_string();
    }
    format!("{code} {first}")
}

#[test]
#[ignore = "needs the nf-core test workflows and the network; run `just run-test-workflows-test`"]
fn pipelines_run_as_recorded() {
    let test_workflows = PathBuf::from(
        std::env::var("NF_CORE_TEST_WORKFLOWS").expect("NF_CORE_TEST_WORKFLOWS is set"),
    )
    .join("pipelines");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_test_workflows.txt");
    let expected = std::fs::read_to_string(&fixture).expect("reads the fixture");
    let expected: Vec<&str> = expected
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    let mut pipelines: Vec<String> = std::fs::read_dir(&test_workflows)
        .expect("reads the test workflows")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    pipelines.sort();
    // Each run has a cache of its own. Downloads are shared with the user's cache, so that
    // runs do not fetch them again.
    let cache = tempfile::tempdir().expect("a temporary directory");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME is set");
    let downloads = home.join(".cache/iunctus/downloads");
    std::fs::create_dir_all(&downloads).expect("creates the downloads directory");
    let run = |pipeline: &String| {
        let cache = cache.path().join(pipeline);
        std::fs::create_dir_all(cache.join("iunctus")).expect("creates the cache");
        std::os::unix::fs::symlink(&downloads, cache.join("iunctus/downloads"))
            .expect("links the downloads");
        format!("{pipeline} {}", outcome(&test_workflows, pipeline, &cache))
    };
    // The runs go in parallel, `IUNCTUS_TEST_WORKFLOWS_JOBS` at a time (4 by default).
    let jobs: usize = std::env::var("IUNCTUS_TEST_WORKFLOWS_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or(4);
    let next = AtomicUsize::new(0);
    let outcomes = Mutex::new(vec![String::new(); pipelines.len()]);
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(pipeline) = pipelines.get(index) else {
                        break;
                    };
                    let line = run(pipeline);
                    outcomes.lock().expect("no run panicked")[index] = line;
                }
            });
        }
    });
    let actual = outcomes.into_inner().expect("no run panicked");
    // A pipeline may only fail with a record, and a record is only kept while it fails.
    let reasons = reasons(EVIDENCE);
    let name = |line: &String| line.split(' ').next().unwrap_or_default().to_owned();
    let unexplained: Vec<&String> = actual
        .iter()
        .filter(|line| !line.ends_with(" ok") && !reasons.contains_key(name(line).as_str()))
        .collect();
    let running: Vec<&&str> = reasons
        .keys()
        .filter(|pipeline| actual.contains(&format!("{pipeline} ok")))
        .collect();
    if actual != expected {
        let changed: Vec<String> = actual
            .iter()
            .filter(|line| !expected.contains(&line.as_str()))
            .cloned()
            .collect();
        panic!(
            "{} pipelines differ:\n{}\n\nThe new fixture:\n{}",
            changed.len(),
            changed.join("\n"),
            actual.join("\n")
        );
    }
    assert!(
        unexplained.is_empty(),
        "pipelines fail without a record in run_evidence.txt of why they cannot run:\n{}",
        unexplained
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        running.is_empty(),
        "pipelines of run_evidence.txt run now; remove their records:\n{}",
        running
            .iter()
            .map(|pipeline| **pipeline)
            .collect::<Vec<_>>()
            .join("\n")
    );
}
