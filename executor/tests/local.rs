//! Runs tasks in containers with the local Docker. Needs Docker and the `debian:stable-slim`
//! image; run with `cargo test -p executor --test local -- --ignored`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use executor::{
    Backend, ContainerEngine, ExecutorError, Finished, Job, LocalContainer, Pool, Staged, Staging,
};
use task_model::{
    ContainerSpec, Digest, FORMAT, FileKind, Image, InputFile, OutputRule, Placement, TaskSpec,
};
use tokio_util::sync::CancellationToken;

fn spec(image: Option<&str>, outputs: &[&str]) -> TaskSpec {
    TaskSpec {
        run_as: None,
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
        containers: Vec::new(),
        replaces_entrypoint: false,
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
    LocalContainer::new(engine, root.join("scratch"), Pool::new(2, 1 << 30))
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
fn runs_container_task() {
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
fn provides_writable_home() {
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
fn preserves_changed_inputs() {
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
fn accepts_path_colons() {
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
fn uses_absolute_workdir() {
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
fn enforces_mount_limits() {
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
    let started = std::time::Instant::now();
    let finished =
        run_with(&executor, "task-8", &task, &Staging::default(), &placement).expect("runs");
    assert_ne!(finished.exit_code, Some(0));
    // The wrapper passes the stop on to the tool, so the task ends without the stop's grace.
    assert!(started.elapsed().as_secs() < 8, "{:?}", started.elapsed());
    let said = std::fs::read_to_string(finished.stderr()).expect("reads");
    assert!(said.contains("longer than its time"), "{said}");
    // A tool that ends well while the engine stops it still ran longer than its time.
    task.arguments = vec!["/bin/bash".into(), "-c".into(), "sleep 3".into()];
    let finished = run_with(
        &executor,
        "task-8-again",
        &task,
        &Staging::default(),
        &placement,
    )
    .expect("runs");
    assert_eq!(finished.exit_code, Some(143));
    // A tool that the kernel kills for lack of memory ends the task, which says so. When the
    // wrapper itself survives, the cgroup told it about the kill; when the kernel killed the
    // wrapper too, the note says that memory may be the cause.
    task.arguments = vec![
        "/bin/bash".into(),
        "-c".into(),
        "head -c 400M /dev/zero | tail > /dev/null".into(),
    ];
    let placement = Placement {
        cpus: 1,
        memory_bytes: Some(32 << 20),
        ..Placement::default()
    };
    let finished = run_with(
        &executor,
        "task-memory",
        &task,
        &Staging::default(),
        &placement,
    )
    .expect("runs");
    assert_ne!(finished.exit_code, Some(0));
    let said = std::fs::read_to_string(finished.stderr()).expect("reads");
    let got = (finished.exit_code, &finished.usage, &said);
    assert!(said.contains("memory"), "{got:?}");
    if let Some(usage) = finished.usage {
        assert!(usage.oom_kills.is_some_and(|kills| kills > 0), "{usage:?}");
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn pins_image_digests() {
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
#[ignore = "needs Docker and the images bash:4.4, alpine:latest and argoproj/argosay:v2"]
fn uses_image_entrypoint() {
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
    // A command that replaces the entrypoint, as the `command` of an Argo container does, runs
    // without it: argosay's entrypoint is /argosay, which fails on `/argosay /argosay ...`.
    let mut task = spec(Some("argoproj/argosay:v2"), &["out.txt"]);
    task.arguments = vec![
        "/argosay".into(),
        "echo".into(),
        "hi".into(),
        "out.txt".into(),
    ];
    task.replaces_entrypoint = true;
    let finished = run(&executor, "task-11", &task, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("reads");
    assert_eq!(output, "hi\n");
    task.replaces_entrypoint = false;
    let finished = run(&executor, "task-12", &task, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(1));
}

/// A container of a task that runs several, which runs a shell script.
fn container(name: &str, image: &str, script: &str, after: &[&str]) -> ContainerSpec {
    ContainerSpec {
        replaces_entrypoint: false,
        service: false,
        name: name.to_owned(),
        image: Some(Image {
            reference: image.to_owned(),
            platform: None,
        }),
        arguments: vec!["sh".into(), "-c".into(), script.to_owned()],
        workdir: None,
        environment: BTreeMap::new(),
        after: after.iter().map(|name| (*name).to_owned()).collect(),
        attempts: 1,
    }
}

/// A task of several containers, whose first container is the main one.
fn several(containers: Vec<ContainerSpec>, outputs: &[&str]) -> TaskSpec {
    let main = &containers[0];
    TaskSpec {
        image: main.image.clone(),
        arguments: main.arguments.clone(),
        containers,
        ..spec(None, outputs)
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim and alpine:latest images"]
fn shares_ordered_folders() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let mut retried = container(
        "c",
        "alpine:latest",
        "[ -e /workspace/tried ] || { touch /workspace/tried; exit 3; }; echo c",
        &["a"],
    );
    retried.attempts = 2;
    let spec = several(
        vec![
            container(
                "main",
                "debian:stable-slim",
                "cat /workspace/message > out.txt; echo main",
                &["b", "c"],
            ),
            container(
                "a",
                "debian:stable-slim",
                "echo a > /workspace/message",
                &[],
            ),
            container("b", "alpine:latest", "echo b >> /workspace/message", &["a"]),
            retried,
        ],
        &["out.txt", "/workspace/"],
    );
    let staged = Staging {
        directories: vec!["/workspace".into()],
        ..Staging::default()
    };
    let finished = run(&executor, "task-set", &spec, &staged).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let read = |path: &std::path::Path| std::fs::read_to_string(path).expect("reads");
    assert_eq!(read(&finished.local_path("out.txt")), "a\nb\n");
    assert_eq!(read(&finished.stdout()), "main\n");
    assert!(finished.local_path("/workspace/tried").exists());
    let control = finished.root.join(executor::CONTROL);
    let ended = executor::containers::ended(&spec, &control);
    let codes: Vec<(&str, Option<i32>)> = (ended.iter())
        .map(|ended| (ended.name.as_str(), ended.exit_code))
        .collect();
    let all = [
        ("main", Some(0)),
        ("a", Some(0)),
        ("b", Some(0)),
        ("c", Some(0)),
    ];
    assert_eq!(codes, all);
    assert_eq!(read(ended[3].stdout.as_ref().expect("a log")), "c\n");
}

#[test]
#[ignore = "needs rootless Docker/Podman, cgroup v2 delegation, and UWE_TEST_NONROOT_IMAGE with USER nobody and Debian tools"]
fn recovers_rootless_outputs() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    assert!(executor.engine.rootless, "requires a rootless engine");
    let secondary = std::env::var("UWE_TEST_NONROOT_IMAGE").expect("a USER nobody image");
    let inspected = runtime()
        .block_on(executor.engine.client().inspect_image(&secondary))
        .unwrap();
    assert_eq!(inspected.config.unwrap().user.as_deref(), Some("nobody"));
    let me = std::fs::metadata(root.path()).unwrap().uid();
    let mut task = several(
        vec![
            container(
                "main",
                "debian:stable-slim",
                "cat /workspace/message > /workspace/out.txt; cat /sys/fs/cgroup/cpu.max > /workspace/main.cpu; cat /sys/fs/cgroup/memory.max > /workspace/main.memory",
                &["secondary"],
            ),
            container(
                "secondary",
                &secondary,
                "echo secondary > /workspace/message; cat /sys/fs/cgroup/cpu.max > /workspace/secondary.cpu; cat /sys/fs/cgroup/memory.max > /workspace/secondary.memory",
                &[],
            ),
        ],
        &["/workspace/"],
    );
    task.run_as = Some(task_model::RunAs::default());
    let staged = Staging {
        directories: vec!["/workspace".into()],
        ..Staging::default()
    };
    let placement = Placement {
        cpus: 1,
        memory_bytes: Some(256 << 20),
        time_seconds: Some(30),
        ..Placement::default()
    };
    let finished = run_with(
        &executor,
        "rootless-mixed-groups",
        &task,
        &staged,
        &placement,
    )
    .expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let read = |name: &str| {
        std::fs::read_to_string(finished.local_path(&format!("/workspace/{name}"))).unwrap()
    };
    assert_eq!(read("out.txt"), "secondary\n");
    assert!(
        !executor::access::foreign(&finished.root, me),
        "all artifacts must belong to the caller"
    );
    let cpu = |name| {
        read(name)
            .split_whitespace()
            .map(|part| part.parse::<u64>().unwrap())
            .collect::<Vec<_>>()
    };
    let main = cpu("main.cpu");
    let secondary = cpu("secondary.cpu");
    assert_eq!(main[1], secondary[1]);
    assert!(
        main[0] + secondary[0] <= main[1],
        "combined CPU quota exceeds one CPU"
    );
    let memory = read("main.memory").trim().parse::<u64>().unwrap()
        + read("secondary.memory").trim().parse::<u64>().unwrap();
    assert_eq!(memory, placement.memory_bytes.unwrap());
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn skips_failed_dependencies() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let spec = several(
        vec![
            container("main", "debian:stable-slim", "echo main > ran", &["a"]),
            container("a", "debian:stable-slim", "echo a >&2; exit 4", &[]),
            container("b", "debian:stable-slim", "echo b", &[]),
        ],
        &["ran"],
    );
    let finished = run(&executor, "task-set-failed", &spec, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(1));
    assert!(finished.files.is_empty());
    assert!(!finished.local_path("ran").exists());
    let said = std::fs::read_to_string(finished.stderr()).expect("reads");
    assert!(
        said.contains("did not run, since container a failed"),
        "{said}"
    );
    let control = finished.root.join(executor::CONTROL);
    let ended = executor::containers::ended(&spec, &control);
    assert_eq!(ended[0].exit_code, Some(1));
    assert_eq!(ended[1].exit_code, Some(4));
    assert_eq!(ended[2].exit_code, Some(0));
}

/// What a task wrote into `out.txt` of its work directory.
fn written(finished: &Finished) -> String {
    std::fs::read_to_string(finished.local_path("out.txt")).expect("reads the output")
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn limits_container_permissions() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let mut task = spec(Some("debian:stable-slim"), &["out.txt"]);
    let script = "grep -E '^(CapEff|NoNewPrivs)' /proc/self/status > \"$OUT\"";
    task.arguments = vec!["sh".into(), "-c".into(), script.into()];
    task.environment.insert(
        "OUT".into(),
        task_model::EnvironmentValue::Literal("out.txt".into()),
    );
    // As the user who runs iunctus: no capabilities, and no way to gain privileges.
    let finished = run(&executor, "least-1", &task, &Staging::default()).expect("runs");
    assert_eq!(
        written(&finished),
        "CapEff:\t0000000000000000\nNoNewPrivs:\t1\n"
    );
    // As root of the image: only CHOWN, to hand the files back, on a rootful engine.
    let mut task = spec(Some("debian:stable-slim"), &["/out.txt"]);
    task.arguments = vec![
        "sh".into(),
        "-c".into(),
        "grep -E '^(CapEff|NoNewPrivs)' /proc/self/status > /out.txt".into(),
    ];
    task.run_as = Some(task_model::RunAs::default());
    let finished = run(&executor, "least-2", &task, &Staging::default()).expect("runs");
    let text = std::fs::read_to_string(finished.local_path("/out.txt")).expect("reads");
    let rootless = executor.engine.rootless;
    let capabilities = if rootless {
        "0000000000000000"
    } else {
        "0000000000000001"
    };
    assert_eq!(text, format!("CapEff:\t{capabilities}\nNoNewPrivs:\t1\n"));
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn uses_image_identity() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let me = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(root.path()).unwrap());
    // As root of the image, in its working directory, with its home, writing outside the
    // folders of the task, as Argo runs it.
    let mut task = spec(Some("debian:stable-slim"), &["/result.txt", "/data/deep"]);
    let script = "pwd > /result.txt; echo \"$HOME\" >> /result.txt; mkdir -p /data/deep && \
                  echo x > /data/deep/f";
    task.arguments = vec!["sh".into(), "-c".into(), script.into()];
    task.run_as = Some(task_model::RunAs::default());
    let finished = run(&executor, "image-1", &task, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let text = std::fs::read_to_string(finished.local_path("/result.txt")).expect("reads");
    assert_eq!(text, "/\n/root\n");
    assert!(finished.local_path("/data/deep").join("f").exists());
    assert!(
        !executor::access::foreign(&finished.root, me),
        "the files are the user's"
    );
    // As another user that the task names, starting in its `workdir`: what it made is
    // handed back to the user who runs iunctus.
    let mut task = spec(Some("debian:stable-slim"), &["/tmp/made"]);
    let script = "id -u > /tmp/id && mkdir -p /tmp/made/deeper && chmod 700 /tmp/made/deeper && \
                  mv /tmp/id /tmp/made/deeper/id && pwd > /tmp/made/where";
    task.arguments = vec!["sh".into(), "-c".into(), script.into()];
    task.workdir = Some("/tmp".into());
    task.run_as = Some(task_model::RunAs {
        user: Some(4242),
        group: Some(4242),
    });
    let finished = run(&executor, "image-2", &task, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let made = finished.local_path("/tmp/made");
    let id = std::fs::read_to_string(made.join("deeper/id")).expect("reads");
    assert_eq!(id, "4242\n");
    assert_eq!(
        std::fs::read_to_string(made.join("where")).expect("reads"),
        "/tmp\n"
    );
    assert!(
        !executor::access::foreign(&finished.root, me),
        "the files are the user's"
    );
}

#[test]
#[ignore = "needs Docker and the argoproj/argosay:v2 image"]
fn replaces_set_entrypoint() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let me = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(root.path()).unwrap());
    let mut first = container("first", "argoproj/argosay:v2", "", &[]);
    first.arguments = vec![
        "/argosay".into(),
        "echo".into(),
        "hi".into(),
        "/tmp/said".into(),
    ];
    first.replaces_entrypoint = true;
    let mut main = container(
        "main",
        "argoproj/argosay:v2",
        "cp /tmp/said /out.txt",
        &["first"],
    );
    main.replaces_entrypoint = true;
    let mut task = several(vec![main, first], &["/out.txt"]);
    // The containers run as root of the image, as Argo runs them, and hand their files back.
    task.run_as = Some(task_model::RunAs::default());
    let finished = run(&executor, "set-1", &task, &Staging::default()).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let text = std::fs::read_to_string(finished.local_path("/out.txt")).expect("reads");
    assert_eq!(text, "hi\n");
    assert!(
        !executor::access::foreign(&finished.root, me),
        "the files are the user's"
    );
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn releases_failed_daemons() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let mut spec = spec(Some("debian:stable-slim"), &[]);
    spec.arguments = vec!["sh".into(), "-c".into(), "exit 3".into()];
    spec.replaces_entrypoint = true;
    let daemon = r#"{"probe":{"check":{"command":["false"]},"period_seconds":1}}"#;
    spec.environment.insert(
        executor::daemon::DAEMON_ENV.into(),
        task_model::EnvironmentValue::Literal(daemon.into()),
    );
    let name = format!("failed-daemon-{}", std::process::id());
    let result = run(&executor, &name, &spec, &Staging::default());
    assert!(result.is_err(), "a daemon that ends is never ready");
    let release = Backend::release(&executor);
    let limit = std::time::Duration::from_secs(60);
    let released = runtime().block_on(async { tokio::time::timeout(limit, release).await });
    assert!(released.is_ok(), "release waits for no stop");
    let engine = runtime()
        .block_on(ContainerEngine::find())
        .expect("an engine");
    let found = runtime().block_on(
        engine
            .client()
            .inspect_container(&format!("iunctus-{name}"), None),
    );
    assert!(found.is_err(), "the container of the daemon is gone");
}

/// A daemon task of `image` that runs `script`, with the daemon settings `daemon`.
fn daemon_spec(image: &str, script: &str, daemon: &str) -> TaskSpec {
    let mut spec = spec(Some(image), &[]);
    spec.arguments = vec!["sh".into(), "-c".into(), script.into()];
    spec.replaces_entrypoint = true;
    spec.environment.insert(
        executor::daemon::DAEMON_ENV.into(),
        task_model::EnvironmentValue::Literal(daemon.into()),
    );
    spec
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn daemons_keep_time() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = executor(root.path());
    let placement = Placement {
        time_seconds: Some(2),
        ..Placement::default()
    };
    let stamp = std::process::id();
    let spec = daemon_spec("debian:stable-slim", "sleep 300", "{}");
    let name = format!("timed-daemon-{stamp}");
    let served = run_with(&executor, &name, &spec, &Staging::default(), &placement);
    assert!(served.expect("a running daemon").address.is_some());
    let engine = runtime()
        .block_on(ContainerEngine::find())
        .expect("an engine");
    let container = format!("iunctus-{name}");
    let running = || {
        let found = runtime().block_on(engine.client().inspect_container(&container, None));
        found.ok().and_then(|found| found.state?.running) == Some(true)
    };
    let begun = std::time::Instant::now();
    while running() && begun.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(!running(), "the daemon stops at the end of its time");
    // A daemon that never gets ready fails at the end of its time.
    let probe = r#"{"probe":{"check":{"command":["false"]},"period_seconds":1}}"#;
    let spec = daemon_spec("debian:stable-slim", "sleep 300", probe);
    let begun = std::time::Instant::now();
    let name = format!("unready-daemon-{stamp}");
    let error = run_with(&executor, &name, &spec, &Staging::default(), &placement).unwrap_err();
    assert!(error.to_string().contains("not ready within"), "{error}");
    assert!(begun.elapsed() < std::time::Duration::from_secs(30));
    let release = Backend::release(&executor);
    let limit = std::time::Duration::from_secs(60);
    runtime()
        .block_on(async { tokio::time::timeout(limit, release).await })
        .expect("released");
}

#[test]
#[ignore = "needs Docker and the python:3.13-slim image"]
fn probes_through_ports() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut executor = executor(root.path());
    // As with a rootless engine, whose container addresses this machine cannot reach.
    executor.probe_ports = true;
    let stamp = std::process::id();
    let server = "python3 -m http.server 8080";
    let placement = Placement {
        time_seconds: Some(20),
        ..Placement::default()
    };
    let probes = [
        (
            r#"{"http":{"port":8080,"path":"/","headers":[{"name":"X-Probe","value":"yes"}]}}"#,
            true,
        ),
        (r#"{"tcp":{"port":8080}}"#, true),
        (r#"{"tcp":{"port":9999}}"#, false),
    ];
    for (index, (check, ready)) in probes.into_iter().enumerate() {
        let daemon = format!(r#"{{"probe":{{"check":{check},"period_seconds":1}}}}"#);
        let spec = daemon_spec("python:3.13-slim", server, &daemon);
        let name = format!("probed-daemon-{stamp}-{index}");
        let served = run_with(&executor, &name, &spec, &Staging::default(), &placement);
        assert_eq!(served.is_ok(), ready, "{check}: {served:?}");
    }
    let release = Backend::release(&executor);
    let limit = std::time::Duration::from_secs(60);
    runtime()
        .block_on(async { tokio::time::timeout(limit, release).await })
        .expect("released");
}
