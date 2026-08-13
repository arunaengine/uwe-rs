//! Runs Nextflow scripts through the engine of `workflow-graph` with the simulator.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{EdgeKind, ExecutionGraph, Launch, Simulator, execute};

const PIPELINE: &str = r#"
process GREET {
    container 'debian:stable-slim'
    cpus 2
    memory '1 GB'
    input:
    val name
    output:
    path "${name}.txt"
    script:
    """
    echo ${params.greeting} ${params.count} ${name} > ${name}.txt
    """
}

process SHOUT {
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
    SHOUT(GREET('world'))
}
"#;

const CONFIG: &str = "params.greeting = 'hi'\nparams.count = 1\nenv {\n    MODE = 'fast'\n}\n";

/// Runs the pipeline in `directory` and returns its graph.
fn run(directory: &Path, launch: Launch) -> ExecutionGraph {
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(directory.join("simulator"));
    execute("main.nf", driver.as_mut(), &mut executor)
        .into_result()
        .expect("the run succeeds")
}

#[test]
fn tasks_and_their_files_make_the_graph() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PIPELINE).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), CONFIG).expect("writes the config");
    let launch = Launch {
        target: directory.path().to_owned(),
        ..Launch::default()
    };
    let graph = run(directory.path(), launch);
    graph.check().expect("a valid graph");
    assert_eq!(graph.steps(), ["GREET", "SHOUT"]);
    let greet = &graph.nodes[0].task;
    assert_eq!(greet.image.as_deref(), Some("debian:stable-slim"));
    assert_eq!(greet.resources.cpus, Some(2.0));
    assert_eq!(greet.resources.memory_bytes, Some(1 << 30));
    assert_eq!(greet.environment["MODE"], "fast");
    assert!(
        greet
            .script
            .as_deref()
            .is_some_and(|script| script.contains("echo hi 1 world"))
    );
    let shout = &graph.nodes[1].task;
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].from, greet.id);
    assert_eq!(graph.edges[0].to, shout.id);
    assert_eq!(
        graph.edges[0].kind,
        EdgeKind::File {
            path: "world.txt".into()
        }
    );
    let document = directory
        .path()
        .join(format!("simulator/tes/{}.json", greet.id));
    assert!(document.is_file());
}

#[test]
fn stage_in_mode_copy_gives_copies_the_task_may_change() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = PIPELINE.replace(
        "process SHOUT {\n",
        "process SHOUT {\n    stageInMode 'copy'\n",
    );
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), CONFIG).expect("writes the config");
    let launch = Launch {
        target: directory.path().to_owned(),
        ..Launch::default()
    };
    let graph = run(directory.path(), launch);
    let writable = |task: usize| {
        let inputs = &graph.nodes[task].task.inputs;
        let staged = inputs.iter().find(|input| input.content.is_none());
        staged.map(|input| input.writable)
    };
    assert_eq!(writable(1), Some(workflow_graph::Writable::Copy));
    let script = &graph.nodes[1].task.inputs[0];
    assert_eq!(script.writable, workflow_graph::Writable::No);
}

#[test]
fn params_come_from_the_params_file_and_the_command_line() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PIPELINE).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), CONFIG).expect("writes the config");
    let file = directory.path().join("params.json");
    std::fs::write(&file, r#"{"greeting": "hello", "count": 2}"#).expect("writes the params");
    let launch = Launch {
        target: directory.path().join("main.nf"),
        inputs: Some(file),
        params: vec![("count".into(), "3".into())],
        ..Launch::default()
    };
    let graph = run(directory.path(), launch);
    let script = graph.nodes[0].task.script.clone().unwrap_or_default();
    assert!(script.contains("echo hello 3 world"), "{script}");
}

#[test]
fn entry_workflows_are_not_supported_yet() {
    let launch = Launch {
        target: "main.nf".into(),
        targets: vec!["ALIGN".into()],
        ..Launch::default()
    };
    let error = nextflow_runtime::driver(&launch).err().expect("an error");
    assert!(error.unsupported);
}

/// In a simulated run, an output that a stub surely does not make is left out, as in Nextflow's
/// stub run: an optional one is then missing, and a required one fails the task with
/// Nextflow's message.
#[test]
fn simulated_runs_leave_out_outputs_that_stubs_do_not_make() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let pipeline = r#"
process SPLIT {
    output:
    path "*.aligned.fasta", emit: aligned
    path "*.unaligned.fasta", emit: unaligned, optional: true
    path "*.log", emit: log
    path "./db/", emit: db
    path ".command.*", emit: command
    script:
    "split"
    stub:
    "mkdir -p db\ntouch s.aligned.fasta s.log"
}

process USE {
    input:
    path fasta
    output:
    path 'used.txt'
    script:
    "cat ${fasta} > used.txt"
}

workflow {
    SPLIT()
    USE(SPLIT.out.unaligned.mix(SPLIT.out.log))
}
"#;
    std::fs::write(directory.path().join("main.nf"), pipeline).expect("writes the script");
    let launch = Launch {
        target: directory.path().to_owned(),
        stub: true,
        simulated: true,
        ..Launch::default()
    };
    let graph = run(directory.path(), launch.clone());
    // USE runs once, for the log, and not for the unaligned sequences.
    assert_eq!(graph.steps(), ["SPLIT", "USE"]);
    let use_task = &graph.nodes[1].task;
    let inputs: Vec<&str> = use_task
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect();
    assert!(inputs.contains(&"s.log"), "{inputs:?}");
    assert_eq!(graph.nodes.len(), 2);
    // A stub that makes no log fails the task.
    let pipeline = pipeline.replace("touch s.aligned.fasta s.log", "touch s.aligned.fasta");
    assert!(pipeline.contains("mkdir -p db"));
    std::fs::write(directory.path().join("main.nf"), pipeline).expect("writes the script");
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(directory.path().join("simulator2"));
    let error = execute("main.nf", driver.as_mut(), &mut executor)
        .into_result()
        .expect_err("the run fails");
    assert!(
        error
            .to_string()
            .contains("Missing output file(s) `*.log` expected by process `SPLIT`"),
        "{error}"
    );
}

/// A process whose outputs decide the next tasks: `splitText` reads one, and `flatten` spreads
/// the files of a wildcard pattern. A sample sheet of the user decides tasks too, but exactly.
const PLANNED: &str = r#"
process SPLIT {
    output:
    path 'rows.txt', emit: rows
    path 'chunk_*.txt', emit: chunks
    path 'report.txt', emit: report
    script:
    """
    printf 'a\\nb\\n' > rows.txt
    touch chunk_1.txt chunk_2.txt report.txt
    """
}

process ROW {
    input:
    val row
    script:
    "echo ${row}"
}

process CHUNK {
    input:
    path chunk
    script:
    "cat ${chunk}"
}

workflow {
    channel.fromPath("${projectDir}/samples.txt").splitText().map { line -> line.trim() }.view()
    SPLIT()
    ROW(SPLIT.out.rows.splitText().map { line -> line.trim() })
    CHUNK(SPLIT.out.chunks.flatten())
    SPLIT.out.report.flatten().view { file -> file.name }
}
"#;

#[test]
fn outputs_that_decide_the_next_tasks_mark_their_task() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PLANNED).expect("writes the script");
    std::fs::write(directory.path().join("samples.txt"), "s1\ns2\n").expect("writes the sheet");
    let launch = Launch {
        target: directory.path().join("main.nf"),
        ..Launch::default()
    };
    let graph = run(directory.path(), launch);
    let steps: Vec<&str> = graph
        .nodes
        .iter()
        .map(|node| node.task.step.as_str())
        .collect();
    assert_eq!(steps, ["SPLIT", "CHUNK", "CHUNK", "ROW", "ROW"]);
    let reasons: Vec<&str> = graph.nodes[0]
        .markers
        .iter()
        .map(|marker| marker.reason.as_str())
        .collect();
    assert_eq!(
        reasons,
        [
            "the workflow read `rows.txt`, an output of this task, to plan the next tasks",
            "the output pattern `chunk_*.txt` matched 2 files, and each started tasks of its own",
        ]
    );
    // Only the first task made files that decide tasks.
    assert_eq!(graph.marked().count(), 1);
}
