//! Runs `iunctus graph` and `iunctus run -graph` on a small Nextflow pipeline, and checks the
//! default executor, which writes a TES task document for each task.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::process::Command;

use workflow_graph::{EdgeKind, ExecutionGraph, Language, SIMULATION, StepGraph};

/// Two processes: `SHOUT` reads the file that `GREET` makes.
const PIPELINE: &str = r#"
process GREET {
    container 'debian:stable-slim'
    input:
    val name
    output:
    path "${name}.txt"
    script:
    """
    echo hello ${name} > ${name}.txt
    """
}

process SHOUT {
    container 'debian:stable-slim'
    input:
    path text
    output:
    path 'shout.txt'
    script:
    """
    tr a-z A-Z < ${text} > shout.txt
    """
}

workflow {
    SHOUT(GREET('world')).view { file -> "made ${file.name}" }
}
"#;

/// Runs the binary in `directory`; the result has the exit code, standard output and error.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_iunctus"))
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs iunctus");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn pipeline() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PIPELINE).expect("writes the script");
    directory
}

/// Checks that the graph holds both tasks and the file between them.
fn check(graph: &ExecutionGraph) {
    graph.check().expect("a valid graph");
    assert_eq!(graph.language, Some(Language::Nextflow));
    assert_eq!(graph.steps(), ["GREET", "SHOUT"]);
    let (greet, shout) = (&graph.nodes[0], &graph.nodes[1]);
    assert_eq!(greet.exit_code, Some(0));
    assert_eq!(graph.edges.len(), 1);
    let edge = &graph.edges[0];
    assert_eq!((edge.from, edge.to), (greet.task.id, shout.task.id));
    assert_eq!(
        edge.kind,
        EdgeKind::File {
            path: "world.txt".into()
        }
    );
}

#[test]
fn graph_prints_the_tasks_and_their_files() {
    let directory = pipeline();
    let (code, stdout, stderr) =
        iunctus(directory.path(), &["graph", "main.nf", "-format", "json"]);
    assert_eq!(code, 0, "{stderr}");
    let graph: ExecutionGraph = serde_json::from_str(&stdout).expect("the graph as JSON");
    check(&graph);
    assert_eq!(graph.simulation.as_deref(), Some(SIMULATION));
    // What the workflow prints goes to standard error, so that the graph stays readable, and
    // so does the note that the run is simulated.
    assert_eq!(stderr, format!("made shout.txt\niunctus: {SIMULATION}\n"));
    let (code, stdout, _) = iunctus(directory.path(), &["graph", ".", "-format", "dot"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("digraph workflow {"), "{stdout}");
    assert!(stdout.contains("[label=\"world.txt\"]"), "{stdout}");
    let (code, stdout, _) = iunctus(directory.path(), &["graph", "main.nf"]);
    assert_eq!(code, 0);
    assert!(
        stdout.starts_with(&format!("{SIMULATION}\n2 tasks, 2 steps, 1 edges\n")),
        "{stdout}"
    );
}

#[test]
fn runs_write_the_graph_and_tes_documents() {
    let directory = pipeline();
    let arguments = ["run", "main.nf", "-graph", "graph.json"];
    let (code, stdout, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout,
        "[simulated] GREET\n[simulated] SHOUT\nmade shout.txt\n"
    );
    let text = std::fs::read_to_string(directory.path().join("graph.json")).expect("the graph");
    let graph: ExecutionGraph = serde_json::from_str(&text).expect("the graph as JSON");
    check(&graph);
    // The simulator keeps one task document per task under the scratch directory.
    let runs = directory.path().join("cache/iunctus/scratch/simulator");
    let run = std::fs::read_dir(&runs)
        .expect("reads the runs")
        .next()
        .expect("a run")
        .expect("an entry")
        .path();
    let id = graph.nodes[0].task.id;
    let document = std::fs::read_to_string(run.join(format!("tes/{id}.json"))).expect("a task");
    let document: serde_json::Value = serde_json::from_str(&document).expect("JSON");
    assert_eq!(document["executors"][0]["image"], "debian:stable-slim");
    assert_eq!(document["inputs"][0]["path"], "/work/.command.sh");
    let arguments = ["run", "main.nf", "-graph", "graph.dot"];
    let (code, _, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{stderr}");
    let dot = std::fs::read_to_string(directory.path().join("graph.dot")).expect("the graph");
    assert!(dot.starts_with("digraph workflow {"), "{dot}");
}

#[test]
fn runs_take_a_pending_limit_and_an_order() {
    let directory = pipeline();
    let arguments = [
        "graph",
        "main.nf",
        "-format",
        "json",
        "-max-pending",
        "4",
        "-order",
        "shuffled:3",
    ];
    let (code, stdout, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{stderr}");
    let graph: ExecutionGraph = serde_json::from_str(&stdout).expect("the graph as JSON");
    check(&graph);
}

#[test]
fn languages_can_be_named() {
    let directory = pipeline();
    std::fs::rename(
        directory.path().join("main.nf"),
        directory.path().join("pipeline.groovy"),
    )
    .expect("renames the script");
    let (code, _, stderr) = iunctus(directory.path(), &["graph", "pipeline.groovy"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("cannot tell the language"), "{stderr}");
    let arguments = ["graph", "pipeline.groovy", "-lang", "nextflow"];
    let (code, stdout, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("\n2 tasks"), "{stdout}");
}

#[test]
fn unknown_executors_and_formats_are_errors() {
    let directory = pipeline();
    let (code, _, stderr) = iunctus(directory.path(), &["run", "main.nf", "-executor", "slurm"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown executor `slurm`"), "{stderr}");
    let (code, _, stderr) = iunctus(directory.path(), &["graph", "main.nf", "-format", "png"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown graph format `png`"), "{stderr}");
    let (code, _, stderr) = iunctus(directory.path(), &["graph", "main.nf", "-view", "nodes"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown graph view `nodes`"), "{stderr}");
}

#[test]
fn graphs_have_a_step_view_and_svg() {
    let directory = pipeline();
    let arguments = ["graph", "main.nf", "-view", "steps"];
    let (code, stdout, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout,
        format!(
            "{SIMULATION}\n2 steps, 1 edges, 2 tasks\nGREET: 1 task <- []\nSHOUT: 1 task <- [GREET]\n"
        )
    );
    let arguments = ["graph", "main.nf", "-view", "steps", "-format", "json"];
    let (code, stdout, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{stderr}");
    let steps: StepGraph = serde_json::from_str(&stdout).expect("the step graph as JSON");
    assert_eq!(steps.steps.len(), 2);
    assert_eq!(
        (steps.edges[0].from.as_str(), steps.edges[0].to.as_str()),
        ("GREET", "SHOUT")
    );
    let (code, stdout, stderr) = iunctus(directory.path(), &["graph", "main.nf", "-format", "svg"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("<svg"), "{stdout}");
    assert!(
        stdout.contains(&format!(">{SIMULATION}</text>")),
        "{stdout}"
    );
    assert!(stdout.contains("id=\"t1\""), "{stdout}");
    let arguments = ["run", "main.nf", "-graph", "steps.svg", "-view", "steps"];
    let (code, _, stderr) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{stderr}");
    let svg = std::fs::read_to_string(directory.path().join("steps.svg")).expect("the SVG");
    assert!(
        svg.contains(">GREET</text>") && svg.contains(">1 task</text>"),
        "{svg}"
    );
}
