//! Runs tasks in containers with the local Docker. Needs Docker and the `debian:stable-slim`
//! image; run with `cargo test -p executor --test local -- --ignored`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use executor::{
    Backend, ContainerEngine, ExecutorError, Finished, Job, LocalContainer, Pool, Staged, Staging,
};
use task_model::{Digest, FORMAT, FileKind, Image, InputFile, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

fn spec(image: Option<&str>, outputs: &[&str]) -> TaskSpec {
    TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: image.map(|reference| Image {
            reference: reference.to_owned(),
            platform: None,
        }),
        arguments: vec!["/bin/bash".into(), "-ue".into(), ".command.sh".into()],
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: outputs
            .iter()
            .map(|pattern| OutputRule {
                pattern: (*pattern).to_owned(),
            })
            .collect(),
        stdin: None,
    }
}

fn staging(inputs: Vec<Staged>) -> Staging {
    Staging {
        inputs,
        ..Staging::default()
    }
}

/// An input with this text, linked from a folder of its own.
fn stage(root: &Path, name: &str, text: &str) -> Staged {
    let folder = root.join(format!("source-{}", name.replace('/', "-")));
    std::fs::create_dir_all(&folder).expect("makes the folder");
    let source = folder.join("content");
    std::fs::write(&source, text).expect("writes the input");
    Staged {
        path: name.to_owned(),
        source,
        directory: false,
        copy: false,
        executable: false,
        ..Default::default()
    }
}

/// One runtime for all tests, on which the client of the engine runs.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime")
    })
}

fn executor(root: &Path) -> LocalContainer {
    let engine = runtime()
        .block_on(ContainerEngine::find())
        .expect("an engine");
    LocalContainer {
        engine,
        scratch: root.join("scratch"),
        pool: Pool::new(2, 1 << 30),
    }
}

/// Runs a job to its end.
fn run(
    executor: &LocalContainer,
    name: &str,
    spec: &TaskSpec,
    staging: &Staging,
) -> Result<Finished, ExecutorError> {
    let placement = Placement {
        memory_bytes: Some(256 << 20),
        ..Placement::default()
    };
    run_with(executor, name, spec, staging, &placement)
}

fn run_with(
    executor: &LocalContainer,
    name: &str,
    spec: &TaskSpec,
    staging: &Staging,
    placement: &Placement,
) -> Result<Finished, ExecutorError> {
    let job = Job {
        name,
        spec,
        staging,
        placement,
    };
    runtime().block_on(Backend::run(executor, job, &CancellationToken::new()))
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn runs_a_task_in_a_container() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let staged = staging(vec![
        stage(
            root.path(),
            ".command.sh",
            "cat in/data.txt > out.txt\necho done\n",
        ),
        stage(root.path(), "in/data.txt", "hello\n"),
    ]);
    let finished = run(
        &executor,
        "task-1",
        &spec(Some("debian:stable-slim"), &["out.txt"]),
        &staged,
    )
    .expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let paths: Vec<&str> = finished
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["out.txt"]);
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("reads the output");
    assert_eq!(output, "hello\n");
    let said = std::fs::read_to_string(finished.stdout()).expect("reads the standard output");
    assert_eq!(said, "done\n");
    let owner = std::os::unix::fs::MetadataExt::uid(
        &std::fs::metadata(finished.local_path("out.txt")).unwrap(),
    );
    let me = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(root.path()).unwrap());
    assert_eq!(owner, me, "outputs belong to the current user");
    // With cgroups v2, the container's own cgroup tells what the task used.
    if Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        let usage = finished.usage.expect("the usage of the task");
        assert!(
            usage.memory_peak.is_some_and(|bytes| bytes > 0),
            "{usage:?}"
        );
        assert!(usage.cpu_usec.is_some(), "{usage:?}");
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn tasks_have_a_home_they_may_write() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    // The container runs as the current user, whom the image does not know, so its own home
    // would be `/`, which the user cannot write.
    let script = "mkdir -p \"$HOME/.cache\" && echo cached > \"$HOME/.cache/x\"\n\
                  cat \"$HOME/.cache/x\" > out.txt\n";
    let staged = staging(vec![stage(root.path(), ".command.sh", script)]);
    let spec = spec(Some("debian:stable-slim"), &["out.txt"]);
    let finished = run(&executor, "task-home", &spec, &staged).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("reads");
    assert_eq!(output, "cached\n");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn inputs_changed_in_place_keep_the_changes() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).expect("makes the folder");
    std::fs::write(project.join("old.txt"), "old\n").expect("writes");
    // The task runs in the folder itself, as a Snakemake `shell()` call of the Snakefile does.
    let in_place = Staged {
        path: ".".into(),
        source: project.clone(),
        directory: true,
        in_place: true,
        ..Staged::default()
    };
    let spec = TaskSpec {
        arguments: vec![
            "/bin/bash".into(),
            "-c".into(),
            "cat old.txt > new.txt && echo more >> old.txt".into(),
        ],
        ..spec(Some("debian:stable-slim"), &[])
    };
    let finished = run(&executor, "task-in-place", &spec, &staging(vec![in_place])).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let read = |name: &str| std::fs::read_to_string(project.join(name)).expect("reads");
    assert_eq!(read("new.txt"), "old\n");
    assert_eq!(read("old.txt"), "old\nmore\n");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn paths_may_hold_colons() {
    let root = tempfile::tempdir().expect("a temporary directory");
    // As the CWL conformance test colon_in_paths: `docker run -v a:b:c` cannot say this.
    let folder = root.path().join("A:Gln2Cys");
    let executor = executor(&folder);
    let staged = staging(vec![
        stage(&folder, ".command.sh", "cat 'in:put.txt' > 'out:put.txt'\n"),
        stage(&folder, "in:put.txt", "colons\n"),
    ]);
    let spec = spec(Some("debian:stable-slim"), &["out:put.txt"]);
    let finished = run(&executor, "task-colons", &spec, &staged).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out:put.txt")).expect("reads");
    assert_eq!(output, "colons\n");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn reports_failures() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let staged = staging(vec![stage(
        root.path(),
        ".command.sh",
        "echo broken >&2\nexit 4\n",
    )]);
    let finished = run(
        &executor,
        "task-2",
        &spec(Some("debian:stable-slim"), &["out.txt"]),
        &staged,
    )
    .expect("runs");
    assert_eq!(finished.exit_code, Some(4));
    let error = std::fs::read_to_string(finished.stderr()).expect("reads the log");
    assert_eq!(error, "broken\n");
    let missing = run(&executor, "task-3", &spec(None, &[]), &Staging::default()).unwrap_err();
    assert!(matches!(missing, ExecutorError::NoImage));
    let error = run(
        &executor,
        "task-4",
        &spec(Some("iunctus.invalid/none:0"), &[]),
        &staged,
    )
    .unwrap_err();
    assert!(error.retryable(), "{error}");
    // The engine does not pull an image that is not here; the runner pins and pulls first.
    let text = error.to_string();
    assert!(text.contains("could not run the task"), "{text}");
    assert!(text.contains("No such image"), "{text}");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn runs_in_the_workdir_with_absolute_paths_and_standard_input() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let mut task = spec(Some("debian:stable-slim"), &["here.txt", "/tmp/out/"]);
    task.workdir = Some("/var/spool/task".into());
    task.arguments = vec![
        "/bin/bash".into(),
        "-c".into(),
        "pwd > here.txt && mkdir -p /tmp/out && cat /data/in.txt - > /tmp/out/both.txt".into(),
    ];
    task.inputs = vec![InputFile {
        path: "/data/in.txt".into(),
        kind: FileKind::File,
        digest: Digest::sha256(b"in\n"),
        executable: false,
        writable: false,
    }];
    task.stdin = Some(Digest::sha256(b"piped\n"));
    let stdin = root.path().join("stdin");
    std::fs::write(&stdin, "piped\n").expect("writes the standard input");
    let staged = Staging {
        inputs: vec![stage(root.path(), "/data/in.txt", "in\n")],
        directories: Vec::new(),
        stdin: Some(stdin),
    };
    let finished = run(&executor, "task-5", &task, &staged).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let read = |path: &str| std::fs::read_to_string(finished.local_path(path)).expect("reads");
    assert_eq!(read("here.txt"), "/var/spool/task\n");
    assert!(finished.files[1].directory);
    assert_eq!(read("/tmp/out/both.txt"), "in\npiped\n");
    // Without arguments, the task runs the image's default command, `bash` for Debian.
    let mut default = spec(Some("debian:stable-slim"), &[]);
    default.arguments.clear();
    let finished = run(&executor, "task-6", &default, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn linked_inputs_are_read_only_and_limits_hold() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let mut task = spec(Some("debian:stable-slim"), &["out.txt"]);
    task.arguments = vec![
        "/bin/bash".into(),
        "-c".into(),
        "cat in.txt > out.txt; echo changed >> in.txt || echo refused >> out.txt".into(),
    ];
    let staged = staging(vec![stage(root.path(), "in.txt", "kept\n")]);
    let finished = run(&executor, "task-7", &task, &staged).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let read = std::fs::read_to_string(finished.local_path("out.txt")).expect("reads");
    assert_eq!(read, "kept\nrefused\n");
    let source = &staged.inputs[0].source;
    assert_eq!(std::fs::read_to_string(source).expect("reads"), "kept\n");
    // A task that runs longer than its time is stopped, and says so.
    task.arguments = vec!["/bin/bash".into(), "-c".into(), "sleep 30".into()];
    let placement = Placement {
        cpus: 1,
        memory_bytes: Some(64 << 20),
        time_seconds: Some(1),
        ..Placement::default()
    };
    let finished =
        run_with(&executor, "task-8", &task, &Staging::default(), &placement).expect("runs");
    assert_ne!(finished.exit_code, Some(0));
    let said = std::fs::read_to_string(finished.stderr()).expect("reads");
    assert!(said.contains("longer than its time"), "{said}");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn pins_images_to_digests() {
    let executor = executor(&std::env::temp_dir());
    let runtime = runtime();
    let pinned = runtime
        .block_on(Backend::pin(&executor, "debian:stable-slim"))
        .expect("inspects")
        .expect("a digest");
    assert!(pinned.starts_with("debian@sha256:"), "{pinned}");
    let again = runtime
        .block_on(Backend::pin(&executor, &pinned))
        .expect("keeps it");
    assert_eq!(again, Some(pinned.clone()));
}

#[test]
#[ignore = "needs Docker and the bash:4.4 and alpine:latest images"]
fn commands_run_through_the_entrypoint_of_the_image() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    // The entrypoint of the bash image runs Bash with options like `-c`, as `docker run` does.
    let mut task = spec(Some("bash:4.4"), &["out.txt"]);
    task.arguments = vec!["-c".into(), "echo moo > out.txt".into()];
    let finished = run(&executor, "task-9", &task, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("reads");
    assert_eq!(output, "moo\n");
    // An image without Bash runs the wrapper in its `sh`, which collects the outputs too.
    let mut task = spec(Some("alpine:latest"), &["out.txt", "*.log", "missing.txt"]);
    let script = "echo moo > out.txt && echo l > a.log && echo \"$HOME\" > home.log";
    task.arguments = vec!["sh".into(), "-c".into(), script.into()];
    let finished = run(&executor, "task-10", &task, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let paths: Vec<&str> = (finished.files.iter())
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["out.txt", "a.log", "home.log"]);
    let home = std::fs::read_to_string(finished.local_path("home.log")).expect("reads");
    assert!(home.trim_end().ends_with("/home"), "{home}");
}
