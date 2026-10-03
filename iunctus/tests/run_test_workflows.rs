//! Compares simulated pipeline outcomes with recorded fixtures and failure evidence.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use fancy_regex::Regex;
use workflow_graph::ExecutionGraph;

mod common;
mod targets;

/// Why each pipeline that fails cannot run, with the output of Nextflow, recorded by
/// `fixtures/run_evidence.py`.
const EVIDENCE: &str = include_str!("fixtures/run_evidence.txt");

/// Parses pipeline names, typed reasons and Nextflow evidence from record blocks.
/// Panics on missing reasons or evidence; docs/support.md describes the format.
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

/// Normalizes exit/error output and validates successful execution graphs.
/// Runs from `project` with optional test profile; ten-minute timeouts exit with 124.
fn outcome(target: &Path, project: &Path, cache: &Path, profile: bool) -> String {
    let graph = cache.join("graph.json");
    let _ = std::fs::remove_file(&graph);
    let mut command = common::isolated(Command::new("timeout"));
    command
        .args(["600", common::BINARY])
        .args(["run", &target.to_string_lossy()]);
    if profile {
        command.args(["-profile", "test"]);
    }
    let output = command
        .arg("-stub-run")
        .args(["-max-pending", "8", "-order", "shuffled:1"])
        .args(["-graph", &graph.to_string_lossy()])
        .args(["--outdir", &cache.join("out").to_string_lossy()])
        .env("XDG_CACHE_HOME", cache)
        .current_dir(project)
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
    let prefix = format!("{}/", project.display());
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
fn pipelines_match_records() {
    let test_workflows = PathBuf::from(
        std::env::var("NF_CORE_TEST_WORKFLOWS").expect("NF_CORE_TEST_WORKFLOWS is set"),
    );
    runs_as_recorded(
        targets::nf_core(&test_workflows),
        "run_test_workflows.txt",
        false,
    );
}

/// The Nextflow workflows of WorkflowHub (decision 4), by `workflowhub/<id>`: each runs its
/// folder as a project, with `manifest.mainScript` or `main.nf`, or else the main script that
/// its RO-Crate names; with `-profile test`, or without a profile when it has no `test` profile.
#[test]
#[ignore = "needs the WorkflowHub test workflows and the network; run `just run-test-workflows-test`"]
fn workflowhub_matches_records() {
    let test_workflows = PathBuf::from(
        std::env::var("WORKFLOWHUB_TEST_WORKFLOWS").expect("WORKFLOWHUB_TEST_WORKFLOWS is set"),
    );
    let targets = targets::workflowhub(&test_workflows);
    runs_as_recorded(targets, "run_workflowhub.txt", true);
}

/// Runs each target, by name, main script or folder, and project folder, and compares the
/// outcomes with a fixture of `tests/fixtures`. With `fallback`, a workflow without a `test`
/// profile runs without one.
fn runs_as_recorded(targets: Vec<(String, PathBuf, PathBuf)>, fixture: &str, fallback: bool) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let expected = std::fs::read_to_string(&fixture).expect("reads the fixture");
    let expected: Vec<&str> = expected
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    // Each run has a cache of its own. Downloads are shared with the user's cache, so that
    // runs do not fetch them again.
    let cache = tempfile::tempdir().expect("a temporary directory");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME is set");
    let downloads = home.join(".cache/iunctus/downloads");
    std::fs::create_dir_all(&downloads).expect("creates the downloads directory");
    let run = |(name, target, project): &(String, PathBuf, PathBuf)| {
        let cache = cache.path().join(name);
        std::fs::create_dir_all(cache.join("iunctus")).expect("creates the cache");
        std::os::unix::fs::symlink(&downloads, cache.join("iunctus/downloads"))
            .expect("links the downloads");
        // The run starts from a copy of the folder of the workflow, as a clone of a user, so
        // that what it writes there, like `work/`, stays out of the test workflows.
        let copy = cache.join("workflow");
        copy_folder(project, &copy);
        let target = match target.strip_prefix(project) {
            Ok(script) if !script.as_os_str().is_empty() => copy.join(script),
            _ => copy.clone(),
        };
        let mut outcome = outcome(&target, &copy, &cache, true);
        if fallback && outcome.ends_with("Unknown configuration profile: 'test'") {
            outcome = self::outcome(&target, &copy, &cache, false);
        }
        let _ = std::fs::remove_dir_all(&copy);
        format!("{name} {outcome}")
    };
    // The runs go in parallel, `IUNCTUS_TEST_WORKFLOWS_JOBS` at a time (4 by default).
    let jobs: usize = std::env::var("IUNCTUS_TEST_WORKFLOWS_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or(4);
    let next = AtomicUsize::new(0);
    let outcomes = Mutex::new(vec![String::new(); targets.len()]);
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(target) = targets.get(index) else {
                        break;
                    };
                    let line = run(target);
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

/// Copies a folder with its files, links and modes.
fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("creates the copy");
    for entry in std::fs::read_dir(from).expect("reads the folder") {
        let entry = entry.expect("an entry");
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("a file type");
        if kind.is_symlink() {
            let link = std::fs::read_link(entry.path()).expect("reads the link");
            std::os::unix::fs::symlink(link, &target).expect("copies the link");
        } else if kind.is_dir() {
            copy_folder(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copies the file");
        }
    }
}
