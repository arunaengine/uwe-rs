//! Service jobs, as Snakemake 9.27.0 runs `test_service_jobs`: the job with a `service()`
//! output runs at once with the jobs that use it, which end it; the run goes on after them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Attempt, Engine, Executor, Launch, Outcome, OutputFile, Running, Task};

const SNAKEFILE: &str = r#"
rule all:
    input: "test.txt", "test2.txt", "both.txt"

rule a:
    output: service("foo.socket")
    shell: "ln -s /dev/random {output}; sleep 10000"

rule b:
    input: "foo.socket"
    output: "test.txt"
    shell: "head -n1 {input} > {output}"

rule c:
    input: "foo.socket"
    output: "test2.txt"
    shell: "head -n1 {input} > {output}"

rule d:
    input: "test.txt", "test2.txt"
    output: "both.txt"
    shell: "cat {input} > {output}"
"#;

/// Makes the outputs of each task in a folder of its own, and keeps the tasks.
struct Made {
    folder: std::path::PathBuf,
    tasks: Vec<Task>,
}

impl Executor for Made {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = attempt.task.clone();
        let mut files = Vec::new();
        for output in &task.outputs {
            let location = self.folder.join(format!("{}-{output}", task.id));
            std::fs::write(&location, "x\n").expect("writes");
            files.push(OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                ..OutputFile::default()
            });
        }
        self.tasks.push(Task::clone(&task));
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }
}

#[test]
fn a_service_runs_with_the_jobs_that_use_it_in_one_task() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    std::fs::create_dir(&workdir).expect("makes the folder");
    std::fs::write(workdir.join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
    let launch = Launch {
        target: workdir,
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let made = folder.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut executor = Made {
        folder: made,
        tasks: Vec::new(),
    };
    let execution = Engine::new(&mut executor).run("Snakefile", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let steps: Vec<&str> = executor
        .tasks
        .iter()
        .map(|task| task.step.as_str())
        .collect();
    assert_eq!(steps, ["a", "d", "all"]);
    let group = &executor.tasks[0];
    assert_eq!(group.outputs, ["test.txt", "test2.txt"]);
    assert!(group.inputs.iter().all(|input| input.path != "foo.socket"));
    let script = group.script.as_deref().unwrap_or_default();
    assert!(script.contains("kill -TERM \"$service\""), "{script}");
}

const ORDERED: &str = r#"
rule all:
    input: "final.txt"
rule server:
    output: service("service.socket")
    container: "debian:stable-slim"
    shell: "touch service.socket alive; trap 'rm -f alive service.socket; exit' TERM; while true; do sleep 0.1; done"
rule first:
    input: "service.socket"
    output: "first.txt"
    threads: lambda wildcards, attempt: attempt
    retries: 1
    container: "debian:stable-slim"
    shell: "test -e alive; sleep 0.1; echo first > {output}"
rule second:
    input: "service.socket", "first.txt"
    output: ensure("final.txt", non_empty=True)
    container: "debian:stable-slim"
    shell: "test -e alive; cat first.txt > {output}"
"#;

#[test]
fn ordered_service_groups_retry_exit_missing_and_integrity_failures() {
    for failure in ["exit", "missing", "empty", "bridge"] {
        let folder = tempfile::tempdir().unwrap();
        let bridge = failure == "bridge";
        let snakefile = if bridge {
            bridged_snakefile()
        } else {
            ORDERED.into()
        };
        std::fs::write(folder.path().join("Snakefile"), snakefile).unwrap();
        let mut driver = snakemake_runtime::driver(&Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        })
        .unwrap();
        let first = driver.start().unwrap().remove(0);
        assert_eq!(first.step, "server");
        assert_eq!(first.resources.cpus, Some(if bridge { 4.0 } else { 3.0 }));
        assert!(first.inputs.iter().all(|input| {
            !["service.socket", "first.txt", "bridge.txt", "final.txt"]
                .contains(&input.path.as_str())
        }));
        let made = folder.path().join("made");
        std::fs::write(&made, "first\n").unwrap();
        let empty = folder.path().join("empty");
        std::fs::write(&empty, "").unwrap();
        let output = |name: &str, path: &std::path::Path| OutputFile {
            path: name.into(),
            location: path.to_string_lossy().into_owned(),
            ..OutputFile::default()
        };
        let mut files = vec![output("first.txt", &made)];
        if failure == "empty" {
            files.push(output("final.txt", &empty));
        }
        let retry = driver
            .complete(Outcome {
                id: first.id,
                exit_code: if failure == "exit" { 1 } else { 0 },
                files,
                ..Outcome::default()
            })
            .unwrap()
            .remove(0);
        assert_eq!(retry.id, first.id);
        assert_eq!(retry.resources.cpus, Some(if bridge { 5.0 } else { 4.0 }));
        assert_eq!(retry.reuse, workflow_graph::Reuse::Denied);
        assert!(driver.deliveries().is_empty(), "{failure}");
        let mut files = vec![output("first.txt", &made), output("final.txt", &made)];
        if bridge {
            files.push(output("bridge.txt", &made));
        }
        let next = driver
            .complete(Outcome {
                id: first.id,
                files,
                ..Outcome::default()
            })
            .unwrap();
        assert_eq!(driver.deliveries().len(), if bridge { 3 } else { 2 });
        for task in next {
            driver
                .complete(Outcome {
                    id: task.id,
                    ..Outcome::default()
                })
                .unwrap();
        }
        driver.finish().unwrap();
    }
}

fn bridged_snakefile() -> String {
    format!(
        "{}\n{}",
        ORDERED
            .replace(
                "\"service.socket\", \"first.txt\"",
                "\"service.socket\", \"bridge.txt\""
            )
            .replace("cat first.txt > {output}", "cat bridge.txt > {output}"),
        r#"
rule bridge:
    input: "first.txt"
    output: "bridge.txt"
    container: "debian:stable-slim"
    shell: "cat first.txt > {output}"
"#
    )
}

#[test]
#[ignore = "needs a container engine; set IUNCTUS_CONTAINER_TESTS=1 and run with --ignored"]
fn ordered_consumers_execute_with_the_service_alive_in_a_real_container() {
    use std::os::unix::fs::MetadataExt;

    assert_eq!(std::env::var("IUNCTUS_CONTAINER_TESTS").as_deref(), Ok("1"));
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("Snakefile"), bridged_snakefile()).unwrap();
    let mut driver = snakemake_runtime::driver(&Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    })
    .unwrap();
    let group = driver.start().unwrap().remove(0);
    let owner = std::fs::metadata(folder.path()).unwrap();
    let engine = std::env::var("IUNCTUS_ENGINE").unwrap_or_else(|_| "docker".into());
    let spec = executor::process::ContainerSpec {
        engine,
        name: format!(
            "iunctus-test-service-{}",
            folder.path().file_name().unwrap().to_string_lossy()
        ),
        image: group.image.unwrap(),
        user: Some(format!("{}:{}", owner.uid(), owner.gid())),
        entrypoint: Some("/bin/sh".into()),
        command: vec!["-c".into(), group.script.unwrap()],
        mounts: vec![executor::process::Mount {
            source: folder.path().to_path_buf(),
            target: "/work".into(),
            read_only: false,
        }],
        workdir: Some("/work".into()),
        environment: Vec::new(),
        memory: None,
        show_stderr: true,
    };
    executor::process::run_to_end(&spec).unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.path().join("final.txt")).unwrap(),
        "first\n"
    );
    assert!(!folder.path().join("alive").exists());
}
