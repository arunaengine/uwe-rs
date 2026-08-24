//! The SLURM backend against a simulated SLURM on this machine: `sbatch` runs the job script in
//! the background, `squeue` tells whether it still runs, `scancel` stops it, and the cluster
//! keeps no accounting. A simulated Apptainer runs the wrapper without a container, which works
//! because the job sees the files at the same paths. One test runs a real container with
//! Docker and needs the `debian:stable-slim` image; run it with `-- --ignored`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::registry::{ImageConfig, ImageConfigs};
use executor::{Backend, EngineCommand, ExecutorError, Job, Staged, Staging};
use slurm_executor::{Container, PullResources, SlurmBackend};
use task_model::{ContainerSpec, FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

const SBATCH: &str = r#"#!/bin/bash
# Runs the job script in the background, in a session of its own.
state="$(dirname "$0")/state"
mkdir -p "$state"
chdir=. output=/dev/null
for argument in "$@"; do
    case $argument in
        --chdir=*) chdir=${argument#--chdir=} ;;
        --output=*) output=${argument#--output=} ;;
    esac
    script=$argument
done
id=$(( $(cat "$state/last" 2>/dev/null || echo 100) + 1 ))
echo "$id" > "$state/last"
printf '%s\n' "$@" > "$state/$id.options"
# A job on a node that does not see the folders fails at once and leaves no log.
if [ -e "$(dirname "$0")/broken" ]; then
    rm "$(dirname "$0")/broken"
    echo 1 > "$state/$id.exit"
    echo "$id"
    exit 0
fi
(
    cd "$chdir" && setsid bash -c 'echo $$ > "$1"; exec bash "$2"' job "$state/$id.pid" "$script" \
        > "$output" 2>&1
    echo $? > "$state/$id.exit"
) > /dev/null 2>&1 &
echo "$id"
"#;

/// Writes JSON with `--json` when the folder has a file `json`, as SLURM 23.02 does; else it
/// refuses the option, as SLURM before 21.08 does.
const SQUEUE: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
json=
for argument in "$@"; do
    case $argument in
        --jobs=*) jobs=${argument#--jobs=} ;;
        --format=%N) echo node-3; exit 0 ;;
        --json)
            [ -e "$(dirname "$0")/json" ] || { echo "squeue: unrecognized option '--json'" >&2; exit 1; }
            json=yes ;;
    esac
done
mkdir -p "$state"
echo "${json:-text}" >> "$state/squeue"
[ -z "$json" ] || printf '{"jobs": ['
separator=
for id in ${jobs//,/ }; do
    [ -e "$state/$id.options" ] || continue
    if [ ! -e "$state/$id.exit" ]; then job_state=RUNNING
    elif [ -e "$state/$id.cancelled" ]; then job_state=CANCELLED
    elif [ "$(cat "$state/$id.exit")" = 0 ]; then job_state=COMPLETED
    else job_state=FAILED; fi
    if [ -n "$json" ]; then
        printf '%s{"job_id": %s, "job_state": ["%s"]}' "$separator" "$id" "$job_state"
        separator=,
    else
        echo "$id $job_state"
    fi
done
[ -z "$json" ] || printf ']}\n'
"#;

const SCANCEL: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
touch "$state/$1.cancelled"
until [ -s "$state/$1.pid" ]; do sleep 0.1; done
kill -TERM -- "-$(cat "$state/$1.pid")"
"#;

/// A cluster without accounting.
const SACCT: &str = "#!/bin/bash\necho 'sacct: accounting storage is disabled' >&2\nexit 1\n";

/// Runs the command after the image in the folder of `--pwd`, without a container. A pull
/// takes a second, notes itself in `state/pulls` and makes an empty file.
const APPTAINER: &str = r#"#!/bin/bash
if [ "$1" = pull ]; then
    echo "$4" >> "$(dirname "$0")/state/pulls"
    sleep 1
    touch "$3"
    exit 0
fi
while [ $# -gt 0 ]; do
    case $1 in
        --pwd) cd "$2" || exit 255; shift 2 ;;
        --bind) shift 2 ;;
        exec|--*) shift ;;
        *) shift; break ;;
    esac
done
exec "$@"
"#;

/// An Apptainer that cannot get the image.
const BROKEN_APPTAINER: &str = "#!/bin/bash\necho 'FATAL: while making image from oci registry: no such image' >&2\nexit 255\n";

/// The commands of the simulated SLURM in a folder of `root`.
fn cluster(root: &Path, apptainer: &str) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("makes the folder");
    for (name, text) in [
        ("sbatch", SBATCH),
        ("squeue", SQUEUE),
        ("scancel", SCANCEL),
        ("sacct", SACCT),
        ("apptainer", apptainer),
    ] {
        let path = bin.join(name);
        std::fs::write(&path, text).expect("writes the command");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("makes it executable");
    }
    bin
}

fn backend(root: &Path, bin: &Path, container: Container) -> SlurmBackend {
    let mut backend = SlurmBackend::new(root.join("scratch"), container);
    backend.commands = Some(bin.to_owned());
    backend.poll = Duration::from_millis(100);
    backend.options = vec!["--account=lab".into()];
    // The configs of the images of the tests, so that no test asks a registry. `tool:1` runs
    // its arguments through `sh -c`, and has a default command.
    let tool = ImageConfig {
        entrypoint: vec!["/bin/sh".into(), "-c".into()],
        cmd: vec!["echo default > out.txt".into()],
        ..ImageConfig::default()
    };
    let plain = ["debian:stable-slim", "debian@sha256:ab12", "alpine:latest"];
    let known = plain.map(|image| (image.to_owned(), ImageConfig::default()));
    backend.configs = ImageConfigs::known(known.into_iter().chain([("tool:1".into(), tool)]));
    backend
}

fn spec(script: &str) -> TaskSpec {
    TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "debian:stable-slim".into(),
            platform: None,
        }),
        arguments: vec!["/bin/bash".into(), "-c".into(), script.into()],
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out.txt".into(),
        }],
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    }
}

/// An input `in.txt` with this text.
fn staging(root: &Path, text: &str) -> Staging {
    let source = root.join("source").join("in.txt");
    std::fs::create_dir_all(source.parent().expect("a folder")).expect("makes the folder");
    std::fs::write(&source, text).expect("writes the input");
    Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            directory: false,
            copy: false,
            executable: false,
            ..Default::default()
        }],
        ..Staging::default()
    }
}

fn placement() -> Placement {
    Placement {
        cpus: 1,
        memory_bytes: Some(256 << 20),
        ..Placement::default()
    }
}

#[tokio::test]
async fn jobs_run_tasks_and_give_their_outputs() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let spec = spec("tr a-z A-Z < in.txt > out.txt");
    let staging = staging(root.path(), "hello\n");
    let placement = placement();
    let job = Job {
        name: "run-P-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "HELLO\n");
    assert_eq!(finished.files.len(), 1);
    // Job 101 pulled the image.
    let options = std::fs::read_to_string(bin.join("state/102.options")).expect("the options");
    assert!(options.contains("--mem=256M\n--account=lab\n"), "{options}");
}

#[tokio::test]
async fn an_image_pinned_to_its_digest_is_pulled_once_for_all_its_jobs() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    backend.pull = PullResources {
        cpus: 8,
        memory_mb: 32_768,
        time_seconds: 7200,
    };
    let mut spec = spec("echo pinned > out.txt");
    spec.image = Some(Image {
        reference: "debian@sha256:ab12".into(),
        platform: None,
    });
    let (staging, placement) = (Staging::default(), placement());
    let job = |name| Job {
        name,
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let (first, second) = tokio::join!(
        backend.run(job("run-P-7-1"), &cancel),
        backend.run(job("run-P-8-1"), &cancel)
    );
    for finished in [first, second] {
        assert_eq!(finished.expect("the job ends").exit_code, Some(0));
    }
    let pulls = std::fs::read_to_string(bin.join("state/pulls")).expect("a pull");
    assert_eq!(pulls, "docker://debian@sha256:ab12\n");
    let file = root.path().join("scratch/images/sha256-ab12.sif");
    assert!(file.exists());
    let script = std::fs::read_to_string(root.path().join("scratch/run-P-7-1/control/job.sh"))
        .expect("the job script");
    assert!(
        script.contains(&format!("'{}' 'sh'", file.display())),
        "{script}"
    );
    let options = std::fs::read_to_string(bin.join("state/101.options")).expect("the options");
    assert!(
        options.starts_with("--parsable\n--job-name=iunctus-image-sha256-ab12-"),
        "{options}"
    );
    assert!(
        options.contains("--cpus-per-task=8\n--mem=32768M\n--time=0-02:00:00\n--account=lab\n"),
        "{options}"
    );
}

#[tokio::test]
async fn an_image_named_by_a_tag_is_pulled_once_per_run() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let apptainer = || Container::Apptainer(text(&bin.join("apptainer")));
    let spec = spec("echo tagged > out.txt");
    let (staging, placement) = (Staging::default(), placement());
    let job = |name| Job {
        name,
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let first_run = backend(root.path(), &bin, apptainer());
    let (first, second) = tokio::join!(
        first_run.run(job("run-P-7-1"), &cancel),
        first_run.run(job("run-P-8-1"), &cancel)
    );
    for finished in [first, second] {
        assert_eq!(finished.expect("the job ends").exit_code, Some(0));
    }
    let pulls = || std::fs::read_to_string(bin.join("state/pulls")).expect("a pull");
    assert_eq!(pulls(), "docker://debian:stable-slim\n");
    let script = std::fs::read_to_string(root.path().join("scratch/run-P-8-1/control/job.sh"))
        .expect("the job script");
    assert!(!script.contains("docker://"), "{script}");
    assert!(script.contains("/scratch/images/tag-"), "{script}");
    // The tag may have moved since: the next run pulls it again, once.
    let next_run = backend(root.path(), &bin, apptainer());
    let finished = next_run.run(job("run2-P-7-1"), &cancel).await;
    assert_eq!(finished.expect("the job ends").exit_code, Some(0));
    assert_eq!(pulls().lines().count(), 2);
}

#[tokio::test]
async fn a_node_that_does_not_see_the_folders_is_left_out() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    std::fs::write(bin.join("broken"), "").expect("breaks the next job");
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    // A SIF file needs no pull, so the next job is that of the task.
    let mut spec = spec("echo fine > out.txt");
    spec.image = Some(Image {
        reference: "/images/debian.sif".into(),
        platform: None,
    });
    let (staging, placement) = (Staging::default(), placement());
    let job = |name| Job {
        name,
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let error = (backend.run(job("run-P-9-1"), &cancel).await).expect_err("a failure");
    assert!(error.retryable(), "{error}");
    let expected = format!(
        "left no log; do the nodes see {} at the same path? It ran on the node node-3, which \
         the later jobs of this run leave out.",
        root.path().join("scratch/run-P-9-1").display()
    );
    assert!(error.to_string().ends_with(&expected), "{error}");
    let finished = (backend.run(job("run-P-9-2"), &cancel).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let options = std::fs::read_to_string(bin.join("state/102.options")).expect("the options");
    assert!(
        options.contains("--exclude=node-3\n--account=lab\n"),
        "{options}"
    );
}

#[tokio::test]
async fn a_task_ends_at_its_time_to_the_second() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let spec = spec("sleep 30; echo late > out.txt");
    let staging = Staging::default();
    let placement = Placement {
        time_seconds: Some(2),
        ..placement()
    };
    let job = Job {
        name: "run-P-10-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let begun = std::time::Instant::now();
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert!(
        begun.elapsed() < Duration::from_secs(20),
        "{:?}",
        begun.elapsed()
    );
    assert_eq!(finished.exit_code, Some(143));
    let stderr = std::fs::read_to_string(finished.stderr()).expect("the standard error");
    assert!(
        stderr.ends_with("ran longer than its time of 2 seconds\n"),
        "{stderr}"
    );
}

#[tokio::test]
async fn clusters_that_write_json_are_read_as_json() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    std::fs::write(bin.join("json"), "").expect("turns on JSON");
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let spec = spec("sleep 1; echo json > out.txt");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-6-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let asked = std::fs::read_to_string(bin.join("state/squeue")).expect("squeue was asked");
    assert!(asked.lines().all(|line| line == "yes"), "{asked}");
}

#[tokio::test]
async fn failed_tools_end_their_tasks() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let spec = spec("echo broken >&2; exit 3");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-2-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(3));
    let stderr = std::fs::read_to_string(finished.stderr()).expect("the standard error");
    assert_eq!(stderr, "broken\n");
}

#[tokio::test]
async fn a_runtime_that_cannot_start_the_image_fails_the_machine() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), BROKEN_APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let spec = spec("true");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-3-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = (backend.run(job, &CancellationToken::new()).await).expect_err("a failure");
    assert!(error.retryable(), "{error}");
    assert!(
        error.to_string().contains("FATAL: while making image"),
        "{error}"
    );
    assert!(!root.path().join("scratch/run-P-3-1").exists());
}

#[tokio::test]
async fn cancelled_jobs_stop() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let spec = spec("sleep 30");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-4-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let stopper = {
        let cancel = cancel.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            cancel.cancel();
        }
    };
    let (result, ()) = tokio::join!(backend.run(job, &cancel), stopper);
    assert!(
        matches!(result, Err(ExecutorError::Cancelled)),
        "{result:?}"
    );
    let exit = std::fs::read_to_string(bin.join("state/101.exit")).expect("the job ended");
    assert_ne!(exit.trim(), "0");
}

#[tokio::test]
#[ignore = "needs Docker and the debian:stable-slim image"]
async fn docker_runs_the_task_in_its_container() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let engine = EngineCommand::docker("docker");
    let backend = backend(root.path(), &bin, Container::Engine(engine));
    let spec = spec("grep -c . /etc/debian_version > out.txt && cat in.txt >> out.txt");
    let staging = staging(root.path(), "hello\n");
    let placement = placement();
    let job = Job {
        name: "run-P-5-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "1\nhello\n");
}

#[tokio::test]
async fn images_of_registries_run_the_command_through_their_entrypoint() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    // Like Argo's argosay: the arguments go to the entrypoint, and without any the image runs
    // its default command.
    let given = vec!["echo given > out.txt".to_owned()];
    for (number, (arguments, written)) in [(given, "given\n"), (Vec::new(), "default\n")]
        .into_iter()
        .enumerate()
    {
        let spec = TaskSpec {
            image: Some(Image {
                reference: "tool:1".into(),
                platform: None,
            }),
            arguments,
            ..spec("")
        };
        let staging = Staging::default();
        let placement = placement();
        let name = format!("run-E-{number}-1");
        let job = Job {
            name: &name,
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
        assert_eq!(finished.exit_code, Some(0));
        let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
        assert_eq!(output, written);
    }
}

/// A task of two containers in two images: `first` writes a file that `main` reads.
fn several(first: &str) -> TaskSpec {
    let container = |name: &str, image: &str, script: &str, after: &[&str]| ContainerSpec {
        replaces_entrypoint: false,
        name: name.into(),
        image: Some(Image {
            reference: image.into(),
            platform: None,
        }),
        arguments: vec!["sh".into(), "-c".into(), script.into()],
        workdir: None,
        environment: BTreeMap::new(),
        after: after.iter().map(|name| (*name).to_owned()).collect(),
        attempts: 1,
    };
    let main = container(
        "main",
        "debian:stable-slim",
        "cat first.txt in.txt > out.txt",
        &["first"],
    );
    TaskSpec {
        arguments: main.arguments.clone(),
        containers: vec![main, container("first", "alpine:latest", first, &[])],
        ..spec("")
    }
}

#[tokio::test]
async fn containers_of_a_task_run_in_one_job() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let spec = several("echo first > first.txt");
    let staging = staging(root.path(), "hello\n");
    let placement = placement();
    let job = Job {
        name: "run-S-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "first\nhello\n");
    // One job, whose script starts a container for each image.
    let script = std::fs::read_to_string(finished.root.join("control/job.sh")).expect("a script");
    assert_eq!(script.matches(" &\n").count(), 2, "{script}");
    let pulls = std::fs::read_to_string(bin.join("state/pulls")).expect("the pulls");
    assert_eq!(pulls.lines().count(), 2, "{pulls}");
    assert!(!bin.join("state/104.options").exists());
    // A container that fails fails the task, and the one after it does not run.
    let spec = several("exit 5");
    let job = Job {
        name: "run-S-2-1",
        spec: &spec,
        ..job
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(1));
    let control = finished.root.join(executor::CONTROL);
    let codes: Vec<Option<i32>> = (executor::containers::ended(&spec, &control).iter())
        .map(|ended| ended.exit_code)
        .collect();
    assert_eq!(codes, [Some(1), Some(5)]);
}

#[tokio::test]
async fn containers_that_wait_for_a_container_that_died_go_on() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    // The first container kills its wrapper, the shell around it and its group's script, as
    // the kernel kills a container for lack of memory, so that none of them records an end.
    let killer = "p() { cut -d ' ' -f 4 /proc/$1/stat; }; w=$PPID; s=$(p $w); g=$(p $s); \
                  kill -9 $w $s $g";
    let spec = several(killer);
    let staging = staging(root.path(), "hello\n");
    let placement = placement();
    let job = Job {
        name: "run-S-3-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let ran = backend.run(job, &cancel);
    let finished = tokio::time::timeout(Duration::from_secs(60), ran).await;
    let finished = finished
        .expect("the job ends in time")
        .expect("the job ends");
    assert_eq!(finished.exit_code, Some(137));
    let control = finished.root.join(executor::CONTROL);
    let codes: Vec<Option<i32>> = (executor::containers::ended(&spec, &control).iter())
        .map(|ended| ended.exit_code)
        .collect();
    assert_eq!(codes, [Some(1), Some(137)]);
}

#[tokio::test]
#[ignore = "needs Docker and the debian:stable-slim and alpine:latest images"]
async fn docker_runs_containers_of_a_task_in_their_images() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let engine = EngineCommand::docker("docker");
    let backend = backend(root.path(), &bin, Container::Engine(engine));
    let spec = several("grep -c . /etc/alpine-release > first.txt");
    let staging = staging(root.path(), "hello\n");
    let placement = placement();
    let job = Job {
        name: "run-S-5-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "1\nhello\n");
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
