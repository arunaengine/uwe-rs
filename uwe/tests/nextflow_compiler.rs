//! Runs nf-core pipelines with `-profile test` on the simulator, natively and through their
//! compiled wir documents in both completion orders, and compares tasks and published files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as Json;
use workflow_graph::{ExecutionGraph, Task, TaskId};

mod common;

/// The pipelines of the comparison.
const PIPELINES: &[&str] = &["demo", "fetchngs"];

/// The nf-core pipelines that compile fully into documents that the wir verifier accepts,
/// with `-profile test`: those of the comparison and the curated pipelines of flowy.
const COMPILED: &[&str] = &[
    "demo",
    "fetchngs",
    "ampliseq",
    "mag",
    "methylseq",
    "rnaseq",
    "sarek",
    "scrnaseq",
    "smrnaseq",
    "taxprofiler",
    "viralrecon",
];

/// How many pipelines verify, run natively and compiled, give equal tasks in both orders, and
/// publish the same files; demo's MULTIQC stages what `mix` merges in another order.
const RECORDED: Counts = Counts {
    verified: 2,
    native: 2,
    compiled: 2,
    tasks: 1,
    published: 2,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    verified: usize,
    native: usize,
    compiled: usize,
    tasks: usize,
    published: usize,
}

/// What one simulated run gave: its tasks as the resume database sees them, sorted by their
/// fingerprints, and the files below its output folder.
struct Ran {
    tasks: Vec<(String, Json)>,
    files: Vec<String>,
}

type Outcome = Result<Ran, String>;

fn run(project: &Path, folder: &Path, order: &str, compiled: bool) -> Outcome {
    let name = format!("{order}-{}", if compiled { "compiled" } else { "native" });
    let graph = folder.join(format!("{name}.json"));
    let output = folder.join(format!("{name}-out"));
    let mut command = common::isolated(Command::new("timeout"));
    command.args(["600", common::BINARY, "run", "-executor", "simulator"]);
    command.arg(project).args(["-profile", "test", "--outdir"]);
    command.arg(&output);
    if compiled {
        command.arg("-lowered");
    }
    command.args(["-stub-run", "-max-pending", "8", "-order", order]);
    command.arg("-graph").arg(&graph);
    let ran = (command.env("XDG_CACHE_HOME", folder.join("cache")))
        .current_dir(project)
        .output()
        .expect("runs uwe");
    if !ran.status.success() {
        let error = String::from_utf8_lossy(&ran.stderr);
        return Err(error.lines().next().unwrap_or_default().to_owned());
    }
    let text = std::fs::read_to_string(&graph).map_err(|error| error.to_string())?;
    let graph: ExecutionGraph = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    let mut tasks: Vec<(String, Json)> = (graph.nodes.iter())
        .map(|node| {
            let mut task = Task {
                id: TaskId(0),
                after: Vec::new(),
                ..node.task.clone()
            };
            // The files of each run lie in folders of their own; the staged names stay.
            for input in &mut task.inputs {
                let name = input.source.rsplit('/').next().unwrap_or_default();
                input.source = name.to_owned();
            }
            let fingerprint = run_database::fingerprint(&task, &[]);
            (fingerprint, serde_json::to_value(&task).unwrap_or_default())
        })
        .collect();
    tasks.sort_by(|a, b| a.0.cmp(&b.0));
    let database = folder.join("cache/uwe").join(run_database::FILE_NAME);
    let files = published(&output, &database);
    Ok(Ran { tasks, files })
}

/// The files that a run published below a folder, by their paths in it with digits as `0`,
/// so that the times in names like `params_<time>.json` match, sorted: those in the folder and
/// those whose deliveries the run database recorded, as a simulated run copies none.
fn published(folder: &Path, database: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            match entry.path().is_dir() {
                true => pending.push(entry.path()),
                false => files.push(entry.path()),
            }
        }
    }
    let recorded = run_database::Database::open(database).and_then(|db| db.all_deliveries());
    files.extend(
        recorded
            .into_iter()
            .flatten()
            .map(|delivered| PathBuf::from(delivered.target)),
    );
    let mut names: Vec<String> = (files.iter())
        .filter_map(|path| path.strip_prefix(folder).ok())
        .map(|path| path.display().to_string())
        .map(|name| {
            name.chars()
                .map(|letter| if letter.is_ascii_digit() { '0' } else { letter })
                .collect()
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The problems of the document that `uwe lower` prints for a pipeline with `-profile test`.
fn problems(project: &Path, folder: &Path) -> Vec<String> {
    let mut command = common::isolated(Command::new("timeout"));
    command.args(["600", common::BINARY, "lower"]).arg(project);
    command
        .args(["-profile", "test", "--outdir"])
        .arg(folder.join("out"));
    let ran = (command.env("XDG_CACHE_HOME", folder.join("cache")))
        .current_dir(project)
        .output()
        .expect("runs uwe");
    let text = String::from_utf8_lossy(&ran.stdout);
    match wir::parse(&text) {
        Ok(document) => match wir::verify(&document) {
            Ok(()) => Vec::new(),
            Err(problems) => problems.iter().map(ToString::to_string).collect(),
        },
        Err(error) => vec![format!("{error}: {}", String::from_utf8_lossy(&ran.stderr))],
    }
}

/// Why `uwe lower` keeps a pipeline native, or the problems of its document.
fn native(project: &Path, folder: &Path) -> Vec<String> {
    let mut command = common::isolated(Command::new("timeout"));
    command.args(["600", common::BINARY, "lower"]).arg(project);
    command
        .args(["-profile", "test", "--outdir"])
        .arg(folder.join("out"));
    let ran = (command.env("XDG_CACHE_HOME", folder.join("cache")))
        .current_dir(project)
        .output()
        .expect("runs uwe");
    let error = String::from_utf8_lossy(&ran.stderr);
    let kept = error
        .lines()
        .filter(|line| line.contains("keeps the pipeline"));
    let kept: Vec<String> = kept.map(str::to_owned).collect();
    if !kept.is_empty() {
        return kept;
    }
    match wir::parse(&String::from_utf8_lossy(&ran.stdout)) {
        Ok(document) => match wir::verify(&document) {
            Ok(()) => Vec::new(),
            Err(problems) => problems.iter().map(ToString::to_string).collect(),
        },
        Err(error) => vec![format!("{error}: {}", error_head(&ran))],
    }
}

/// The first line of what a run of uwe printed to its standard error.
fn error_head(ran: &std::process::Output) -> String {
    let error = String::from_utf8_lossy(&ran.stderr);
    error.lines().next().unwrap_or_default().to_owned()
}

fn compare(project: &Path, counts: &mut Counts) -> String {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let problems = problems(project, directory.path());
    counts.verified += usize::from(problems.is_empty());
    if let Some(problem) = problems.first() {
        return format!("{} problems, like {problem}", problems.len());
    }
    let mut runs = Vec::new();
    for order in ["asked", "reversed"] {
        let native = run(project, directory.path(), order, false);
        let compiled = run(project, directory.path(), order, true);
        runs.push((native, compiled));
    }
    counts.native += usize::from(runs.iter().all(|(native, _)| native.is_ok()));
    counts.compiled += usize::from(runs.iter().all(|(_, compiled)| compiled.is_ok()));
    let both = |same: fn(&Ran, &Ran) -> bool| {
        runs.iter().all(|run| match run {
            (Ok(native), Ok(compiled)) => same(native, compiled),
            _ => false,
        })
    };
    counts.tasks += usize::from(both(|native, compiled| native.tasks == compiled.tasks));
    counts.published += usize::from(both(|native, compiled| native.files == compiled.files));
    match &runs[0] {
        (Err(error), _) => format!("native run fails: {error}"),
        (Ok(native), Err(error)) => format!(
            "{} native tasks; compiled run fails: {error}",
            native.tasks.len()
        ),
        (Ok(native), Ok(compiled)) => format!(
            "{} native tasks, {} compiled",
            native.tasks.len(),
            compiled.tasks.len()
        ),
    }
}

#[test]
#[ignore = "needs the nf-core test workflows and the network; run `just test-workflows` first"]
fn compiled_pipelines_match() {
    let test_workflows = PathBuf::from(
        std::env::var("NF_CORE_TEST_WORKFLOWS").expect("NF_CORE_TEST_WORKFLOWS is set"),
    );
    let mut counts = Counts::default();
    let mut lines = Vec::new();
    for pipeline in PIPELINES {
        let project = test_workflows.join("pipelines").join(pipeline);
        lines.push(format!("{pipeline}: {}", compare(&project, &mut counts)));
    }
    assert_eq!(counts, RECORDED, "the outcomes:\n{}", lines.join("\n"));
}

#[test]
#[ignore = "needs the nf-core test workflows and the network; run `just test-workflows` first"]
fn pipelines_compile_fully() {
    let test_workflows = PathBuf::from(
        std::env::var("NF_CORE_TEST_WORKFLOWS").expect("NF_CORE_TEST_WORKFLOWS is set"),
    );
    let mut failed = Vec::new();
    for pipeline in COMPILED {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let project = test_workflows.join("pipelines").join(pipeline);
        if let Some(reason) = native(&project, directory.path()).first() {
            failed.push(format!("{pipeline}: {reason}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
