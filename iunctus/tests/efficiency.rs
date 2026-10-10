//! Checks measured resource sizing, task grouping and TES input locality.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// A step that asks for 4 GB and writes the memory limit of its container.
const SCRIPT: &str = r#"
process LIMIT {
    container 'debian:stable-slim'
    memory '4 GB'
    publishDir 'results', mode: 'copy'
    input:
    val n
    output:
    path "limit-${n}.txt"
    script:
    """
    cat /sys/fs/cgroup/memory.max > limit-${n}.txt
    """
}

workflow {
    LIMIT(channel.of(1, 2))
}
"#;

/// Runs the binary in `directory`; the result has the exit code and the standard output and
/// error.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

/// The memory limit that the task of `n` saw in its container, in bytes.
fn limit(directory: &Path, n: u32) -> u64 {
    let path = directory.join(format!("results/limit-{n}.txt"));
    let text = std::fs::read_to_string(&path).expect("reads the limit");
    text.trim().parse().expect("a number of bytes")
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn reuses_observed_memory() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let directory = directory.path();
    std::fs::write(directory.join("main.nf"), SCRIPT).expect("writes the workflow");
    let asked = 4 * 1024 * 1024 * 1024;

    let (code, output) = iunctus(directory, &["run", "main.nf", "-executor", "local"]);
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("asked for less"), "{output}");
    assert_eq!(limit(directory, 1), asked);

    // The cache would answer every task, so the next runs leave it out.
    let again = ["run", "main.nf", "-executor", "local", "-no-cache"];
    let (code, output) = iunctus(directory, &again);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("tasks asked for less than the workflow")
            && output.contains("LIMIT (2 tasks): 67.1 MB of memory instead of 4.3 GB"),
        "{output}"
    );
    assert_eq!(limit(directory, 1), task_model::LEAST_MEMORY);
    assert_eq!(limit(directory, 2), task_model::LEAST_MEMORY);

    let kept = [&again[..], &["-no-right-sizing"]].concat();
    let (code, output) = iunctus(directory, &kept);
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("asked for less"), "{output}");
    assert_eq!(limit(directory, 2), asked);

    // A simulated run says what a real run would ask for.
    let (code, output) = iunctus(
        directory,
        &["run", "-executor", "simulator", "main.nf", "-no-cache"],
    );
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("a real run would ask for less than the workflow"),
        "{output}"
    );
}

/// Twelve short tasks; the third fails.
const SHORT: &str = r#"
process SHORT {
    container 'debian:stable-slim'
    publishDir 'results', mode: 'copy'
    errorStrategy 'ignore'
    input:
    val n
    output:
    path "out-${n}.txt"
    script:
    """
    [ ${n} -ne 3 ] || exit 3
    echo "task ${n}" > out-${n}.txt
    echo "log of ${n}"
    """
}

workflow {
    SHORT(channel.of(1..12))
}
"#;

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn groups_keep_results() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let directory = directory.path();
    std::fs::write(directory.join("main.nf"), SHORT).expect("writes the workflow");
    let (code, output) = iunctus(directory, &["run", "main.nf", "-executor", "local"]);
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("[together]"), "{output}");

    std::fs::remove_dir_all(directory.join("results")).expect("removes the results");
    let again = ["run", "main.nf", "-executor", "local", "-no-cache"];
    let (code, output) = iunctus(directory, &again);
    assert_eq!(code, 0, "{output}");
    // Nextflow commands keep the image entrypoint, so the grouping policy runs them separately.
    assert!(!output.contains("[together]"), "{output}");
    for n in (1..=12).filter(|n| *n != 3) {
        let text = std::fs::read_to_string(directory.join(format!("results/out-{n}.txt")));
        assert_eq!(text.expect("an output").trim(), format!("task {n}"));
    }
    assert!(!directory.join("results/out-3.txt").exists());
    // Each task has its own exit code and log.
    let database = run_database::Database::open(&directory.join("cache/iunctus/runs.sqlite"))
        .expect("opens the run database");
    let workflow = directory.join("main.nf").to_string_lossy().into_owned();
    let run = database
        .last_run(&workflow, true)
        .expect("reads")
        .expect("a run");
    for task in database.tasks(&run.id).expect("reads the tasks") {
        let n = task
            .name
            .trim_start_matches("SHORT (")
            .trim_end_matches(')');
        let detail = database
            .task(&run.id, task.id)
            .expect("reads")
            .expect("a task");
        let log = database.log(&run.id, task.id, 1, "stdout").expect("reads");
        match n {
            "3" => assert_eq!(detail.attempts[0].exit_code, Some(3)),
            n => {
                assert_eq!(detail.attempts[0].exit_code, Some(0));
                assert_eq!(log.expect("a log").text.trim(), format!("log of {n}"));
            }
        }
    }

    let alone = [&again[..], &["-no-grouping"]].concat();
    let (code, output) = iunctus(directory, &alone);
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("[together]"), "{output}");
}

/// Runs short tasks through the runner on the Funnel server of `just funnel-server`.
#[test]
#[ignore = "needs the Funnel server of `just funnel-server`, Docker and debian:stable-slim"]
fn tes_groups_tasks() {
    use std::sync::Arc;
    use task_model::Measured;
    use workflow_graph::{Attempt, CancellationToken, Executor, Task, TaskId};

    let server = std::env::var("IUNCTUS_TES_SERVER").unwrap_or("http://localhost:18000".into());
    // Funnel may read and write below /tmp.
    let folder = tempfile::tempdir_in("/tmp").expect("a temporary directory");
    let folder = folder.path();
    let store = content_store::Store::local(&folder.join("store")).expect("a store");
    let image = "debian:stable-slim";
    let backend = tes_executor::TesBackend {
        client: tes_executor::Client::new(&server, None).expect("a client"),
        scratch: folder.join("scratch"),
        // Funnel runs images of this machine, whose configs no registry needs to give.
        configs: executor::registry::ImageConfigs::known([(image.into(), Default::default())]),
        settings: Default::default(),
        staging: None,
    };
    let settings = task_runner::Settings {
        downloads: folder.join("downloads"),
        import_inputs: true,
        storage: storage::Storage::default(),
        digests: None,
    };
    let runner = task_runner::Runner::new(store, backend, "run-t", settings);
    let mut executor = task_runner::RunnerExecutor::new(runner, None);
    // No registry either: the image stays as it is named.
    executor.pin_images(&[(image.to_owned(), image.to_owned())].into());
    let earlier = Measured {
        milliseconds: 10,
        overhead: Some(5_000),
        ..Measured::default()
    };
    executor.measured = Arc::new([("S".to_owned(), vec![earlier])].into());
    executor.together = true;
    let runtime = workflow_graph::runtime().expect("a runtime");
    let running: Vec<_> = (1..=4)
        .map(|n| {
            let task = Task {
                id: TaskId(n),
                name: format!("S ({n})"),
                step: "S".into(),
                image: Some(image.into()),
                command: vec![
                    "bash".into(),
                    "-c".into(),
                    format!("sleep 0.5; echo {n} > out.txt; echo log {n}"),
                ],
                outputs: vec!["out.txt".into()],
                replaces_entrypoint: true,
                ..Task::default()
            };
            let attempt = Attempt {
                task: Arc::new(task),
                number: 1,
                cancel: CancellationToken::new(),
            };
            runtime.spawn(executor.run(attempt))
        })
        .collect();
    let mut outcomes: Vec<_> = running
        .into_iter()
        .map(|handle| runtime.block_on(handle).expect("ends").expect("an outcome"))
        .collect();
    for (n, outcome) in (1..).zip(&outcomes) {
        assert_eq!(outcome.exit_code, 0, "{outcome:?}");
        assert_eq!(outcome.stdout.trim(), format!("log {n}"));
        let text = std::fs::read_to_string(&outcome.files[0].location).expect("reads the output");
        assert_eq!(text.trim(), n.to_string());
    }
    // One after another in one container: no tool started before the one before it ended.
    outcomes.sort_by_key(|outcome| outcome.started);
    for pair in outcomes.windows(2) {
        assert!(pair[1].started >= pair[0].ended, "{pair:?}");
    }
}

/// The number of tasks that the TES server at `server`, like `http://localhost:18000`, knows.
fn tes_tasks(server: &str) -> usize {
    use std::io::{Read, Write};
    let address = server.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(address).expect("connects to the server");
    let request = format!("GET /v1/tasks?view=MINIMAL HTTP/1.0\r\nHost: {address}\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("asks");
    let mut answer = String::new();
    stream
        .read_to_string(&mut answer)
        .expect("reads the answer");
    let body = answer.split_once("\r\n\r\n").expect("a body").1;
    let tasks: serde_json::Value = serde_json::from_str(body).expect("JSON");
    tasks["tasks"].as_array().map_or(0, Vec::len)
}

/// Counts tasks routed near inputs across two local Funnel servers.
/// Run with `--test-threads 1`.
#[test]
#[ignore = "needs two Funnel servers, at ports 18000 and 18001, Docker and debian:stable-slim"]
fn routes_near_inputs() {
    use std::sync::Arc;
    use workflow_graph::{Attempt, CancellationToken, Executor, Input, Task, TaskId};

    let first = std::env::var("IUNCTUS_TES_SERVER").unwrap_or("http://localhost:18000".into());
    let second = "http://localhost:18001";
    let (before_first, before_second) = (tes_tasks(&first), tes_tasks(second));
    let folder = tempfile::tempdir_in("/tmp").expect("a temporary directory");
    let folder = folder.path();
    let (here, there) = (folder.join("here"), folder.join("there"));
    for data in [&here, &there] {
        std::fs::create_dir_all(data).expect("makes the data folder");
        std::fs::write(data.join("a.txt"), "a\n").expect("writes the data");
    }
    let image = "debian:stable-slim";
    let backend = |server: &str| tes_executor::TesBackend {
        client: tes_executor::Client::new(server, None).expect("a client"),
        scratch: folder.join("scratch"),
        configs: executor::registry::ImageConfigs::known([(image.into(), Default::default())]),
        settings: Default::default(),
        staging: None,
    };
    let backends = task_runner::Sites {
        default: backend(&first),
        sites: [("there".to_owned(), backend(second))].into(),
    };
    let settings = task_runner::Settings {
        downloads: folder.join("downloads"),
        import_inputs: true,
        storage: storage::Storage::default(),
        digests: None,
    };
    let store = content_store::Store::local(&folder.join("store")).expect("a store");
    let runner = task_runner::Runner::new(store, backends, "run-s", settings);
    let mut executor = task_runner::RunnerExecutor::new(runner, None);
    executor.pin_images(&[(image.to_owned(), image.to_owned())].into());
    executor.sites = task_runner::SiteRule {
        sites: vec![task_runner::Site {
            name: "there".into(),
            near: vec![there.to_string_lossy().into_owned()],
        }],
    };
    let runtime = workflow_graph::runtime().expect("a runtime");
    let running: Vec<_> = [&here, &here, &there, &there]
        .into_iter()
        .enumerate()
        .map(|(n, data)| {
            let task = Task {
                id: TaskId(n as u64),
                name: format!("S ({n})"),
                step: "S".into(),
                image: Some(image.into()),
                command: vec!["bash".into(), "-c".into(), format!("cat a.txt; echo {n}")],
                inputs: vec![Input {
                    path: "a.txt".into(),
                    source: data.join("a.txt").to_string_lossy().into_owned(),
                    ..Input::default()
                }],
                ..Task::default()
            };
            let attempt = Attempt {
                task: Arc::new(task),
                number: 1,
                cancel: CancellationToken::new(),
            };
            runtime.spawn(executor.run(attempt))
        })
        .collect();
    for (n, handle) in running.into_iter().enumerate() {
        let outcome = runtime.block_on(handle).expect("ends").expect("an outcome");
        assert_eq!(outcome.exit_code, 0, "{outcome:?}");
        assert_eq!(outcome.stdout, format!("a\n{n}\n"));
    }
    assert_eq!(tes_tasks(&first) - before_first, 2);
    assert_eq!(tes_tasks(second) - before_second, 2);
}
