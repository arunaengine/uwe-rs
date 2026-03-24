//! Runs Nextflow scripts through the engine of `workflow-graph` with the mock TES executor.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{EdgeKind, ExecutionGraph, Launch, MockTes, execute};

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
    let mut executor = MockTes::new(directory.join("mock"));
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
    let document = directory.path().join(format!("mock/tes/{}.json", greet.id));
    assert!(document.is_file());
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
