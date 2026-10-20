//! Runs Nextflow scripts through the engine of `workflow-graph` with the simulator.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::json;
use workflow_graph::{Driver, EdgeKind, ExecutionGraph, Launch, Simulator, execute};

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
fn rejects_secret_values() {
    struct Secrets;
    impl nextflow_evaluator::interpreter::Host for Secrets {
        fn print(&mut self, _: &str) {}
        fn secret(
            &mut self,
            name: &str,
        ) -> Result<Option<String>, nextflow_evaluator::interpreter::Error> {
            Ok((name == "TOKEN").then(|| "fixture".to_owned()))
        }
    }
    for read in [
        "secrets.TOKEN",
        "secrets['TOKEN']",
        "secrets.getProperty('TOKEN')",
        "secrets.getAt('TOKEN')",
    ] {
        for configured in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(
                directory.path().join("main.nf"),
                format!("workflow {{ def value = {read}; assert secrets.MISSING == null }}"),
            )
            .unwrap();
            std::fs::write(
                directory.path().join("nextflow.config"),
                if configured {
                    format!("params.token = {read}")
                } else {
                    String::new()
                },
            )
            .unwrap();
            let mut driver = nextflow_runtime::driver::Nextflow::new(
                &Launch {
                    target: directory.path().to_owned(),
                    ..Launch::default()
                },
                nextflow_runtime::driver::Settings {
                    host: Box::new(Secrets),
                    ..Default::default()
                },
            )
            .unwrap();
            let error = driver.start().unwrap_err();
            assert!(error.unsupported);
            assert!(error.message.contains("process `secret` directive"));
            assert!(!error.message.contains("fixture"));
        }
    }
}

#[test]
fn missing_env_empty() {
    struct Missing;
    impl nextflow_evaluator::interpreter::Host for Missing {
        fn print(&mut self, _: &str) {}
        fn secret(
            &mut self,
            _: &str,
        ) -> Result<Option<String>, nextflow_evaluator::interpreter::Error> {
            Ok(None)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.nf"),
        "process P {\n script:\n 'true'\n}\nworkflow { P() }",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("nextflow.config"),
        "env.TOKEN = secrets.MISSING\n",
    )
    .unwrap();
    let mut driver = nextflow_runtime::driver::Nextflow::new(
        &Launch {
            target: directory.path().to_owned(),
            ..Default::default()
        },
        nextflow_runtime::driver::Settings {
            host: Box::new(Missing),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(driver.start().unwrap()[0].environment["TOKEN"], "");
}

#[test]
fn builds_task_graph() {
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

#[cfg(unix)]
#[test]
fn bin_helpers_run() {
    use std::os::unix::fs::PermissionsExt;

    for status in [0, 17] {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project 'space");
        let bin = project.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let helper = bin.join("helper-tool");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nprintf 'ready\\n'\nexit {status}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            project.join("main.nf"),
            "process HELP {\n container 'debian:stable-slim'\n script:\n 'helper-tool'\n}\nworkflow { HELP() }",
        )
        .unwrap();
        let mut driver = nextflow_runtime::driver(&Launch {
            target: project,
            ..Launch::default()
        })
        .unwrap();
        let task = driver.start().unwrap().remove(0);
        let work = directory.path().join("work 'space");
        std::fs::create_dir(&work).unwrap();
        for input in &task.inputs {
            let path = work.join(&input.path);
            if input.directory {
                std::os::unix::fs::symlink(&input.source, &path).unwrap();
            } else {
                std::fs::write(path, input.content.as_ref().unwrap()).unwrap();
            }
        }
        let result = std::process::Command::new(&task.command[0])
            .args(&task.command[1..])
            .current_dir(work)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(status), "{:?}", result);
        assert_eq!(result.stdout, b"ready\n");
    }
}

#[test]
fn copies_staged_inputs() {
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
fn combines_launch_parameters() {
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

/// The check reads `--count 3` as the integer of the schema, but the script gets the text, as
/// in Nextflow 26.04.6, where `params.count + 1` is '31'.
#[test]
fn keeps_parameter_texts() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let pipeline = PIPELINE.replace("${params.count}", "${params.count + 1}");
    std::fs::write(directory.path().join("main.nf"), pipeline).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), CONFIG).expect("writes the config");
    std::fs::write(
        directory.path().join("nextflow_schema.json"),
        r#"{"type":"object","properties":{"count":{"type":"integer"}}}"#,
    )
    .expect("writes the schema");
    let launch = Launch {
        target: directory.path().to_owned(),
        params: vec![("count".into(), "3".into())],
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let inputs = driver.inputs().expect("describes inputs").expect("inputs");
    assert_eq!(inputs.values["count"], json!("3"));
    let validation = input_schema::validate(&inputs, false);
    assert!(validation.diagnostics.is_empty());
    assert_eq!(validation.values["count"], json!(3));
    driver
        .use_inputs(validation.values)
        .expect("applies normalized inputs");

    let mut executor = Simulator::new(directory.path().join("simulator"));
    let graph = execute("main.nf", driver.as_mut(), &mut executor)
        .into_result()
        .expect("the run succeeds");
    let script = graph.nodes[0].task.script.as_deref().unwrap_or_default();
    assert!(script.contains("echo hi 31 world"), "{script}");
}

/// Nextflow 26.04.6 runs legacy syntax, like a `for` loop, only with `NXF_SYNTAX_PARSER=v1`,
/// which gives the config and the script `--count 3 --flag TRUE` as 3 and true.
#[test]
fn types_legacy_params() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let pipeline = PIPELINE
        .replace(
            "${params.count}",
            "${params.count + 1} ${params.flag.getClass().simpleName} ${params.twice}",
        )
        .replace(
            "SHOUT(GREET('world'))",
            "for (name in ['world']) {\n        SHOUT(GREET(name))\n    }",
        );
    std::fs::write(directory.path().join("main.nf"), pipeline).expect("writes the script");
    let config = format!("{CONFIG}params.flag = 'no'\nparams.twice = params.count * 2\n");
    std::fs::write(directory.path().join("nextflow.config"), config).expect("writes the config");
    let launch = Launch {
        target: directory.path().to_owned(),
        params: vec![("count".into(), "3".into()), ("flag".into(), "TRUE".into())],
        ..Launch::default()
    };
    let graph = run(directory.path(), launch);
    let script = graph.nodes[0].task.script.as_deref().unwrap_or_default();
    assert!(script.contains("echo hi 4 Boolean 6 world"), "{script}");
}

#[test]
fn selects_named_entry() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), "process P {\n input:\n val x\n script:\n \"echo ${x}\"\n}\nworkflow ALIGN { P('selected') }\nworkflow OTHER { P('other') }\nworkflow { P('default') }\n").unwrap();
    for (entry, step, script) in [
        (None, "P", "echo default"),
        (Some("ALIGN"), "ALIGN:P", "echo selected"),
        (Some("OTHER"), "OTHER:P", "echo other"),
    ] {
        let graph = run(
            directory.path(),
            Launch {
                target: directory.path().to_owned(),
                targets: entry.map(str::to_owned).into_iter().collect(),
                ..Launch::default()
            },
        );
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].task.step, step);
        assert!(
            graph.nodes[0]
                .task
                .script
                .as_ref()
                .unwrap()
                .contains(script)
        );
    }
}

#[test]
fn selects_imported_entry() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.nf"),
        "include { SUB as ALIGN } from './sub'\nworkflow { error 'wrong workflow' }\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("sub.nf"),
        "process P {\n script:\n 'true'\n}\nworkflow SUB { P() }\n",
    )
    .unwrap();
    let graph = run(
        directory.path(),
        Launch {
            target: directory.path().to_owned(),
            targets: vec!["ALIGN".into()],
            ..Launch::default()
        },
    );
    assert_eq!(graph.steps(), ["ALIGN:P"]);
}

#[test]
fn rejects_invalid_entries() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), "workflow ALIGN {}\n").unwrap();
    let mut driver = nextflow_runtime::driver(&Launch {
        target: directory.path().to_owned(),
        targets: vec!["MISSING".into()],
        ..Launch::default()
    })
    .unwrap();
    let error = driver.start().expect_err("unknown entry");
    assert!(
        error
            .to_string()
            .contains("Unknown workflow entry name: MISSING"),
        "{error}"
    );
    let error = nextflow_runtime::driver(&Launch {
        target: directory.path().to_owned(),
        targets: vec!["ALIGN".into(), "OTHER".into()],
        ..Launch::default()
    })
    .err()
    .expect("multiple entries");
    assert!(
        error
            .to_string()
            .contains("Only one Nextflow workflow entry"),
        "{error}"
    );
}

/// In a simulated run, an output that a stub surely does not make is left out, as in Nextflow's
/// stub run: an optional one is then missing, and a required one fails the task with
/// Nextflow's message.
#[test]
fn excludes_absent_outputs() {
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
fn marks_deciding_outputs() {
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

#[test]
fn enables_conda_images() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.nf"), "process P {\n conda 'bioconda::fastqc=0.12.1'\n container 'debian:stable-slim'\n script:\n 'true'\n}\nworkflow { P() }").unwrap();
    for enabled in [false, true] {
        std::fs::write(
            directory.path().join("nextflow.config"),
            format!("conda.enabled = {enabled}\ndocker.registry = 'quay.io'\n"),
        )
        .unwrap();
        let mut driver = nextflow_runtime::driver(&Launch {
            target: directory.path().to_owned(),
            ..Launch::default()
        })
        .unwrap();
        let tasks = driver.start().unwrap();
        let image = tasks[0].image.as_deref().unwrap();
        assert_eq!(
            image.starts_with("conda-env:nextflow-packages:"),
            enabled,
            "{image}"
        );
        if enabled {
            let reference: serde_json::Value =
                serde_json::from_str(image.strip_prefix("conda-env:nextflow-packages:").unwrap())
                    .unwrap();
            assert_eq!(
                reference["channels"],
                serde_json::json!(["conda-forge", "bioconda"])
            );
        }
        if !enabled {
            assert_eq!(image, "quay.io/debian:stable-slim");
        }
    }
}

#[test]
fn conda_config_channels() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("main.nf"),
        "process P {\n conda 'foo=1'\n script:\n 'true'\n}\nworkflow { P() }",
    )
    .unwrap();
    for channels in ["['custom', 'bioconda']", "'custom,bioconda'"] {
        std::fs::write(
            root.path().join("nextflow.config"),
            format!("conda.enabled = true\nconda.channels = {channels}\n"),
        )
        .unwrap();
        let mut driver = nextflow_runtime::driver(&Launch {
            target: root.path().to_owned(),
            ..Default::default()
        })
        .unwrap();
        let task = driver.start().unwrap().remove(0);
        let source = task.image.unwrap();
        let value: serde_json::Value =
            serde_json::from_str(source.strip_prefix("conda-env:nextflow-packages:").unwrap())
                .unwrap();
        assert_eq!(value["channels"], serde_json::json!(["custom", "bioconda"]));
    }
}
