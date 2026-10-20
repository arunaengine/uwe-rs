//! Checks the backend with local tools that simulate SLURM submission and scheduling.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::binary::{Binary, Machine};
use executor::registry::{ImageConfig, ImageConfigs};
use executor::remote::RemoteStage;
use executor::{Backend, EngineCommand, ExecutorError, Job, Staged, Staging};
use slurm_executor::{Container, Helper, NodeStaging, PullResources, SlurmBackend};
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
        --job-name=*) name=${argument#--job-name=} ;;
    esac
    script=$argument
done
exec 9> "$state/ids.lock"
flock -x 9
id=$(( $(cat "$state/last" 2>/dev/null || echo 100) + 1 ))
echo "$id" > "$state/last"
flock -u 9
printf '%s\n' "$@" > "$state/$id.options"
printf '%s\n' "$name" > "$state/$id.name"
printf '%s\n' "$script" > "$state/$id.command"
echo 2026-10-03T00:00:00 > "$state/$id.submitted"
# A node of a shared file system may first see a wrapper that another machine has just
# written as empty: the wrapper is empty for two seconds.
if [ -e "$(dirname "$0")/late-wrapper" ]; then
    rm "$(dirname "$0")/late-wrapper"
    wrapper="$chdir/control/wrapper.sh"
    mv "$wrapper" "$wrapper.whole"
    : > "$wrapper"
    (sleep 2; mv "$wrapper.whole" "$wrapper") > /dev/null 2>&1 &
fi
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
[ ! -e "$(dirname "$0")/missing-response" ] || exec sleep 30
"#;

/// An `sbatch` that hangs before it submits anything, like one that cannot reach the controller.
const HANGING_SBATCH: &str = "#!/bin/bash\nexec sleep 30\n";

/// Writes JSON with `--json` when the folder has a file `json`, as SLURM 23.02 does; else it
/// refuses the option, as SLURM before 21.08 does.
const SQUEUE: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
json=
for argument in "$@"; do
    case $argument in
        --jobs=*) jobs=${argument#--jobs=} ;;
        --name=*) name=${argument#--name=} ;;
        --format=%N) echo node-3; exit 0 ;;
        '--format=%i|%o') commands=yes ;;
        '--format=%i|%T|%j|%V|%o') identified=yes ;;
        '--format=%i %T %r') reasons=yes ;;
        --json)
            [ -e "$(dirname "$0")/json" ] || { echo "squeue: unrecognized option '--json'" >&2; exit 1; }
            json=yes ;;
    esac
done
mkdir -p "$state"
echo "${json:-text}" >> "$state/squeue"
if [ -n "$name" ]; then
    for file in "$state"/*.name; do
        [ "$(cat "$file" 2>/dev/null)" = "$name" ] && jobs="$jobs,$(basename "$file" .name)"
    done
fi
[ -z "$json" ] || printf '{"jobs": ['
separator=
for id in ${jobs//,/ }; do
    [ -e "$state/$id.options" ] || continue
    if [ ! -e "$state/$id.exit" ]; then job_state=RUNNING
    elif [ -e "$state/$id.cancelled" ]; then job_state=CANCELLED
    elif [ "$(cat "$state/$id.exit")" = 0 ]; then job_state=COMPLETED
    else job_state=FAILED; fi
    [ ! -e "$(dirname "$0")/completing" ] || job_state=COMPLETING
    # A file `held` holds the reason of a job that SLURM keeps pending and held.
    reason=None
    if [ -e "$(dirname "$0")/held" ] && [ ! -e "$state/$id.cancelled" ]; then
        job_state=PENDING reason=$(cat "$(dirname "$0")/held")
    fi
    if [ -n "$commands" ]; then
        printf '%s|%s\n' "$id" "$(cat "$state/$id.command")"
    elif [ -n "$identified" ]; then
        printf '%s|%s|%s|%s|%s\n' "$id" "$job_state" "$(cat "$state/$id.name")" "$(cat "$state/$id.submitted")" "$(cat "$state/$id.command")"
    elif [ -n "$json" ]; then
        printf '%s{"job_id": %s, "job_state": ["%s"], "state_description": "%s"}' "$separator" "$id" "$job_state" "$reason"
        separator=,
    elif [ -n "$reasons" ]; then
        echo "$id $job_state $reason"
    else
        echo "$id $job_state"
    fi
done
[ -z "$json" ] || printf ']}\n'
"#;

const SCANCEL: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
for option in "$@"; do
    case $option in --name=*) [ "${option#--name=}" = "$(cat "$state/$1.name")" ] || exit 0 ;; esac
done
printf '%s\n' "$*" >> "$state/scancel.calls"
touch "$state/$1.cancelled"
until [ -s "$state/$1.pid" ]; do sleep 0.1; done
kill -TERM -- "-$(cat "$state/$1.pid")"
"#;

/// A cluster without accounting.
const SACCT: &str = "#!/bin/bash\necho 'sacct: accounting storage is disabled' >&2\nexit 1\n";

/// Runs the command after the image in the folder of `--pwd`, without a container. A pull
/// takes a second, notes itself in `state/pulls` and its web proxy in `state/proxies`, and
/// makes an empty file.
const APPTAINER: &str = r#"#!/bin/bash
if [ "$1" = pull ]; then
    echo "$4" >> "$(dirname "$0")/state/pulls"
    [ -z "$HTTPS_PROXY" ] || echo "$HTTPS_PROXY $http_proxy" >> "$(dirname "$0")/state/proxies"
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

/// An Apptainer whose container's record of the task shows late, as a shared file system may
/// show it to another machine: it hides the record for two seconds after the container ended.
const LATE_APPTAINER: &str = r#"#!/bin/bash
while [ $# -gt 0 ]; do
    case $1 in
        --pwd) cd "$2" || exit 255; shift 2 ;;
        --bind) shift 2 ;;
        exec|--*) shift ;;
        *) shift; break ;;
    esac
done
"$@"
status=$?
record="$(dirname "$2")/exit"
mv "$record" "$record.late"
(sleep 2; mv "$record.late" "$record") > /dev/null 2>&1 &
exit $status
"#;

/// Runs the command after the image without a container, as `APPTAINER` does, but sees the
/// folders of the binds at their targets: it rewrites the paths of the wrapper, which runs on a
/// node that does not see the scratch folder.
const NODE_APPTAINER: &str = r#"#!/bin/bash
binds=()
while [ $# -gt 0 ]; do
    case $1 in
        --pwd) start=$2; shift 2 ;;
        --bind) rest=${2#*:}; binds+=("${rest%%:*}" "${2%%:*}"); shift 2 ;;
        exec|--*) shift ;;
        *) shift; break ;;
    esac
done
place() {
    local path=$1 index
    for ((index = 0; index < ${#binds[@]}; index += 2)); do
        case $path in
            "${binds[index]}"|"${binds[index]}"/*)
                printf '%s' "${binds[index + 1]}${path#"${binds[index]}"}"; return ;;
        esac
    done
    printf '%s' "$path"
}
text=$(cat "$(place "$2")")
for ((index = 0; index < ${#binds[@]}; index += 2)); do
    text=${text//"${binds[index]}"/"${binds[index + 1]}"}
done
wrapper=$(mktemp)
printf '%s\n' "$text" > "$wrapper"
cd "$(place "$start")" || exit 255
exec sh "$wrapper"
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
        // A child process writes the command. A process that another test starts meanwhile
        // gets a copy of what this one has open, and a command open for writing cannot run.
        let script = "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"";
        let status = std::process::Command::new("sh")
            .args(["-c", script, "sh", text])
            .arg(bin.join(name))
            .status();
        assert!(status.expect("writes the command").success());
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
async fn rejects_offline_singularity() {
    let root = tempfile::tempdir().expect("a folder");
    let backend = SlurmBackend::new(
        root.path().join("scratch"),
        Container::Apptainer("singularity".into()),
    );
    let spec = spec("true");
    let staging = Staging::default();
    let placement = Placement {
        offline: true,
        ..Placement::default()
    };
    let job = Job {
        name: "offline",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let result = backend.run(job, &CancellationToken::new()).await;
    assert!(
        matches!(result, Err(ExecutorError::Unsupported(_))),
        "{result:?}"
    );
    assert!(!backend.scratch.exists());
}

#[tokio::test]
async fn returns_job_outputs() {
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
async fn pulls_pinned_once() {
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
async fn pulls_tagged_once() {
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
async fn pulls_through_proxy() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    backend.proxy = Some("http://proxy.example.org:3128".into());
    let spec = spec("echo through > out.txt");
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-7-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend.run(job, &CancellationToken::new()).await;
    assert_eq!(finished.expect("the job ends").exit_code, Some(0));
    let proxies = std::fs::read_to_string(bin.join("state/proxies")).expect("a proxy");
    let proxy = "http://proxy.example.org:3128";
    assert_eq!(proxies, format!("{proxy} {proxy}\n"));
}

#[tokio::test]
async fn excludes_invisible_nodes() {
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
async fn enforces_task_deadline() {
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
    // The job after the pull has five more minutes than its task, for staging.
    let options = std::fs::read_to_string(bin.join("state/102.options")).expect("the options");
    assert!(options.contains("--time=0-00:05:02\n"), "{options}");
}

#[tokio::test]
async fn reads_cluster_json() {
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
    assert!(asked.lines().any(|line| line == "yes"), "{asked}");
    assert!(
        asked.lines().all(|line| line == "yes" || line == "text"),
        "{asked}"
    );
    assert_eq!(asked.lines().last(), Some("text"), "{asked}");
}

#[tokio::test]
async fn reports_tool_failures() {
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
async fn reports_runtime_failures() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), BROKEN_APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    // A SIF file needs no pull, so the job of the task meets the broken runtime.
    let mut spec = spec("true");
    spec.image = Some(Image {
        reference: "/images/tool.sif".into(),
        platform: None,
    });
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
async fn bounds_shared_pulls() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), BROKEN_APPTAINER);
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    backend.pull_pause = Duration::from_millis(100);
    let spec = spec("true");
    let (staging, placement) = (Staging::default(), placement());
    let job = |name| Job {
        name,
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let (first, second, third) = tokio::join!(
        backend.run(job("run-P-4-1"), &cancel),
        backend.run(job("run-P-5-1"), &cancel),
        backend.run(job("run-P-6-1"), &cancel)
    );
    for result in [first, second, third] {
        let error = result.expect_err("a failure");
        assert!(
            matches!(error, ExecutorError::MissingImage { .. }),
            "{error}"
        );
        assert!(!error.retryable(), "{error}");
        let text = error.to_string();
        assert!(text.contains("FATAL: while making image"), "{text}");
        assert!(text.contains("tried 3 times"), "{text}");
    }
    // Three pull jobs in all, and no job of a task.
    let jobs = std::fs::read_dir(bin.join("state")).expect("the jobs");
    let names: Vec<String> = (jobs.flatten())
        .filter(|entry| entry.path().extension().is_some_and(|end| end == "options"))
        .map(|entry| std::fs::read_to_string(entry.path()).expect("the options"))
        .collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        names
            .iter()
            .all(|options| options.contains("--job-name=iunctus-image-"))
    );
    // A later task of the run fails at once with the same error.
    let error = (backend.run(job("run-P-7-1"), &cancel).await).expect_err("a failure");
    assert!(error.to_string().contains("tried 3 times"), "{error}");
    let last = std::fs::read_to_string(bin.join("state/last")).expect("the last job");
    assert_eq!(last.trim(), "103");
}

#[tokio::test]
async fn waits_wrapper_completion() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let mut spec = spec("tr a-z A-Z < in.txt > out.txt");
    spec.image = Some(Image {
        reference: "/images/tool.sif".into(),
        platform: None,
    });
    let staging = staging(root.path(), "hello\n");
    let placement = placement();
    let job = Job {
        name: "run-P-5-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    std::fs::write(bin.join("late-wrapper"), "").expect("writes the flag");
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "HELLO\n");
}

#[tokio::test]
async fn waits_late_records() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), LATE_APPTAINER);
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let mut spec = spec("echo late > out.txt; exit 4");
    spec.image = Some(Image {
        reference: "/images/tool.sif".into(),
        platform: None,
    });
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-6-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(4));
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
    let mut spec = spec("sleep 30");
    spec.image.as_mut().unwrap().reference = "/images/tool.sif".into();
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-4-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    // The job is cancelled once it runs and its journal names it.
    let started = bin.join("state/101.pid");
    let journal = backend.scratch.join("run-P-4-1/remote.json");
    let ready =
        || std::fs::metadata(&started).is_ok_and(|data| data.len() > 0) && submitted(&journal);
    let stopper = async {
        until(ready).await;
        cancel.cancel();
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
async fn cancels_held_jobs() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    std::fs::write(bin.join("held"), "launch failed requeued held").expect("holds the jobs");
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let mut spec = spec("sleep 30");
    spec.image.as_mut().unwrap().reference = "/images/tool.sif".into();
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "run-P-12-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let begun = std::time::Instant::now();
    let error = (backend.run(job, &CancellationToken::new()).await).unwrap_err();
    assert!(
        error.to_string().contains("launch failed requeued held"),
        "{error}"
    );
    assert!(begun.elapsed() < Duration::from_secs(20));
    let calls = std::fs::read_to_string(bin.join("state/scancel.calls")).expect("a cancel");
    assert!(calls.starts_with("101 "), "{calls}");
}

#[tokio::test]
async fn recovers_lost_submissions() {
    let root = tempfile::tempdir().unwrap();
    let bin = cluster(root.path(), APPTAINER);
    std::fs::write(bin.join("missing-response"), "").unwrap();
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    backend.submit_grace = Duration::from_secs(2);
    let mut spec = spec("sleep 30");
    spec.image.as_mut().unwrap().reference = "/images/tool.sif".into();
    let (staging, placement) = (Staging::default(), placement());
    let job = || Job {
        name: "ambiguous",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = backend
        .run(job(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.retryable());
    let path = backend.scratch.join("ambiguous/remote.json");
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(record["id"].is_null());
    assert!(backend.scratch.join("ambiguous/control/job.sh").is_file());
    assert!(backend.run(job(), &CancellationToken::new()).await.is_err());
    assert_eq!(
        std::fs::read_to_string(bin.join("state/last"))
            .unwrap()
            .trim(),
        "101"
    );
    // Recovery finds the job by its name and script, and cancels it.
    backend.reconcile().await.unwrap();
    assert!(!path.exists());
    assert!(bin.join("state/101.cancelled").exists());
    // A submission that never reached SLURM is lost, and blocks nothing.
    let status = std::process::Command::new("sh")
        .args(["-c", "printf '%s' \"$1\" > \"$2\"", "sh", HANGING_SBATCH])
        .arg(bin.join("sbatch"))
        .status();
    assert!(status.unwrap().success());
    let job = Job {
        name: "unsent",
        ..job()
    };
    assert!(backend.run(job, &CancellationToken::new()).await.is_err());
    assert!(backend.scratch.join("unsent/remote.json").is_file());
    backend.reconcile().await.unwrap();
    assert!(!backend.scratch.join("unsent/remote.json").exists());
}

#[tokio::test]
async fn recovery_cleans_staging() {
    let root = tempfile::tempdir().unwrap();
    let bin = cluster(root.path(), NODE_APPTAINER);
    std::fs::write(bin.join("missing-response"), "").unwrap();
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    backend.submit_grace = Duration::from_secs(2);
    let objects = content_store::Store::local(&root.path().join("staging")).unwrap();
    // A helper that fails ends the job at once, after it sent its log.
    backend.remote = Some(NodeStaging {
        stage: RemoteStage {
            objects,
            expiry: Duration::from_secs(600),
            max_outputs: 4,
            credentials: false,
            cleanup: true,
        },
        helper: Helper::Path("/bin/false".into()),
        folder: Some(text(&root.path().join("node"))),
        secret_file: None,
    });
    let spec = spec("true");
    let (staging, placement) = (staging(root.path(), "hello\n"), placement());
    let job = Job {
        name: "unclear",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    assert!(backend.run(job, &CancellationToken::new()).await.is_err());
    // The job may still run, so its objects stay, and the journal names them.
    let path = backend.scratch.join("unclear/remote.json");
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let plan = record["staging"]["plan"]
        .as_str()
        .expect("the plan")
        .to_owned();
    let stage = &backend.remote.as_ref().unwrap().stage;
    let contents = stage.objects.objects("cas/").await.unwrap();
    assert!(
        contents.iter().any(|object| object.location == plan),
        "{contents:?}"
    );
    // Recovery finds the job, and then removes the plan and what the job sent back.
    until(|| bin.join("state/101.exit").exists()).await;
    backend.reconcile().await.unwrap();
    assert!(!path.exists());
    let contents = stage.objects.objects("cas/").await.unwrap();
    assert_eq!(contents.len(), 2, "{contents:?}");
    assert!(stage.objects.objects("attempts/").await.unwrap().is_empty());
}

#[tokio::test]
async fn admits_active_owner() {
    let root = tempfile::tempdir().unwrap();
    let bin = cluster(root.path(), APPTAINER);
    let container = Container::Apptainer(text(&bin.join("apptainer")));
    let active = backend(root.path(), &bin, container.clone());
    let peer = backend(root.path(), &bin, container);
    let mut spec = spec("sleep 2; echo owner > out.txt");
    spec.image.as_mut().unwrap().reference = "/images/tool.sif".into();
    let (staging, placement, cancel) = (Staging::default(), placement(), CancellationToken::new());
    let job = Job {
        name: "active",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let concurrent = async {
        let journal = active.scratch.join("active/remote.json");
        for _ in 0..100 {
            if std::fs::read(&journal)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|record| record["identity"].is_object())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let other = Job {
            name: "peer",
            ..job
        };
        peer.run(other, &cancel).await.unwrap();
    };
    let (result, ()) = tokio::join!(active.run(job, &cancel), concurrent);
    assert_eq!(result.unwrap().exit_code, Some(0));
    assert!(!bin.join("state/scancel.calls").exists());
}

#[tokio::test]
async fn recovers_dropped_attempts() {
    let root = tempfile::tempdir().unwrap();
    let bin = cluster(root.path(), APPTAINER);
    let container = Container::Apptainer(text(&bin.join("apptainer")));
    let active = backend(root.path(), &bin, container.clone());
    let peer = backend(root.path(), &bin, container);
    let mut spec = spec("sleep 30");
    spec.image.as_mut().unwrap().reference = "/images/tool.sif".into();
    let (staging, placement, cancel) = (Staging::default(), placement(), CancellationToken::new());
    let old = Job {
        name: "dropped",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let journal = active.scratch.join("dropped/remote.json");
    tokio::select! {
        result = active.run(old, &cancel) => panic!("{result:?}"),
        () = until(|| submitted(&journal)) => {}
    }
    let delay = bin.join("completing");
    std::fs::write(&delay, "").unwrap();
    let mut next = spec.clone();
    next.arguments = vec!["sh".into(), "-c".into(), "echo recovered > out.txt".into()];
    let run = peer.run(
        Job {
            name: "next",
            spec: &next,
            ..old
        },
        &cancel,
    );
    let verify = async {
        // The peer stopped the dropped job, which completes for now.
        let calls = bin.join("state/scancel.calls");
        until(|| std::fs::read_to_string(&calls).is_ok_and(|text| text.contains("dropped"))).await;
        assert!(active.scratch.join("dropped/remote.json").is_file());
        assert!(!peer.scratch.join("next").exists());
        std::fs::remove_file(delay).unwrap();
    };
    let (result, ()) = tokio::join!(run, verify);
    assert_eq!(result.unwrap().exit_code, Some(0));
    assert!(!active.scratch.join("dropped/remote.json").exists());
    assert!(
        std::fs::read_to_string(bin.join("state/scancel.calls"))
            .unwrap()
            .contains("--name=iunctus-dropped")
    );
}

#[tokio::test]
async fn preserves_changed_identity() {
    let root = tempfile::tempdir().unwrap();
    let bin = cluster(root.path(), APPTAINER);
    let container = Container::Apptainer(text(&bin.join("apptainer")));
    let active = backend(root.path(), &bin, container.clone());
    let peer = backend(root.path(), &bin, container);
    let mut spec = spec("sleep 120");
    spec.image.as_mut().unwrap().reference = "/images/tool.sif".into();
    let (staging, placement, cancel) = (Staging::default(), placement(), CancellationToken::new());
    let job = Job {
        name: "original",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let journal = active.scratch.join("original/remote.json");
    tokio::select! {
        result = active.run(job, &cancel) => panic!("{result:?}"),
        () = until(|| submitted(&journal)) => {}
    }
    let lock = std::fs::File::options()
        .write(true)
        .open(active.scratch.join("original/remote.lock"))
        .unwrap();
    until(|| lock.try_lock().is_ok()).await;
    lock.unlock().unwrap();
    drop(lock);
    let bytes = std::fs::read(&journal).unwrap();
    let mut next = spec.clone();
    next.arguments = vec!["sh".into(), "-c".into(), "exit 0".into()];
    let blocked = Job {
        name: "blocked",
        spec: &next,
        ..job
    };
    for (field, changed) in [
        ("name", "replacement"),
        ("submitted", "2026-10-03T00:00:01"),
        ("command", "/another/script"),
    ] {
        let path = bin.join(format!("state/101.{field}"));
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, format!("{changed}\n")).unwrap();
        let result = peer.run(blocked, &cancel).await;
        assert!(
            result.is_err(),
            "changed {field} admitted: {result:?}; original journal: {}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(std::fs::read(&journal).unwrap(), bytes);
        assert!(!bin.join("state/scancel.calls").exists());
        std::fs::write(path, original).unwrap();
    }
    let mut record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    record["configuration"] = serde_json::json!("changed");
    let changed = record.to_string().into_bytes();
    std::fs::write(&journal, &changed).unwrap();
    assert!(peer.run(blocked, &cancel).await.is_err());
    assert_eq!(std::fs::read(&journal).unwrap(), changed);
    assert!(!peer.scratch.join("blocked").exists());
    std::fs::write(&journal, bytes).unwrap();
    peer.reconcile().await.unwrap();
}

#[tokio::test]
async fn completing_retains_ownership() {
    let root = tempfile::tempdir().unwrap();
    let bin = cluster(root.path(), APPTAINER);
    let completing = bin.join("completing");
    std::fs::write(&completing, "").unwrap();
    let backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let mut spec = spec("echo complete > out.txt");
    spec.image.as_mut().unwrap().reference = "/images/tool.sif".into();
    let (staging, placement) = (Staging::default(), placement());
    let job = Job {
        name: "completing",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let verify = async {
        let marker = backend
            .scratch
            .join("completing/control")
            .join(slurm_executor::CONTAINER_EXIT);
        until(|| marker.is_file()).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let journal = backend.scratch.join("completing/remote.json");
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal).unwrap()).unwrap();
        assert_eq!(record["id"], "101");
        std::fs::remove_file(&completing).unwrap();
    };
    let (result, ()) = tokio::join!(backend.run(job, &cancel), verify);
    let finished = result.unwrap();
    assert_eq!(finished.exit_code, Some(0));
    assert!(!finished.root.join("remote.json").exists());
}

#[tokio::test]
async fn recovers_unresolved_pulls() {
    let root = tempfile::tempdir().unwrap();
    let bin = cluster(root.path(), APPTAINER);
    std::fs::write(bin.join("missing-response"), "").unwrap();
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    backend.submit_grace = Duration::from_secs(2);
    let spec = spec("true");
    let (staging, placement) = (Staging::default(), placement());
    let job = |name| Job {
        name,
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let (first, second) = tokio::join!(
        backend.run(job("first"), &cancel),
        backend.run(job("second"), &cancel)
    );
    assert!(matches!(first, Err(ExecutorError::MissingImage { .. })));
    assert!(matches!(second, Err(ExecutorError::MissingImage { .. })));
    assert_eq!(
        std::fs::read_to_string(bin.join("state/last"))
            .unwrap()
            .trim(),
        "101"
    );
    let attempts: Vec<_> = std::fs::read_dir(&backend.scratch)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("remote.json").is_file())
        .collect();
    assert_eq!(attempts.len(), 1);
    assert!(
        attempts[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("image-")
    );
    assert!(attempts[0].join("control/job.sh").is_file());
    backend.reconcile().await.unwrap();
    assert!(!attempts[0].join("remote.json").exists());
}

#[tokio::test]
#[ignore = "needs Docker and the debian:stable-slim image"]
async fn docker_runs_container() {
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
async fn uses_registry_entrypoint() {
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

#[tokio::test]
async fn inspects_pinned_once() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), APPTAINER);
    let container = Container::Apptainer(text(&bin.join("apptainer")));
    let run = async |backend: &SlurmBackend, name: &str| {
        let spec = TaskSpec {
            image: Some(Image {
                reference: "tool@sha256:abcd".into(),
                platform: None,
            }),
            arguments: Vec::new(),
            ..spec("")
        };
        let (staging, placement) = (Staging::default(), placement());
        let job = Job {
            name,
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
        std::fs::read_to_string(finished.local_path("out.txt")).expect("an output")
    };
    let mut first = backend(root.path(), &bin, container.clone());
    let config = ImageConfig {
        entrypoint: vec!["/bin/sh".into(), "-c".into()],
        cmd: vec!["echo kept > out.txt".into()],
        ..ImageConfig::default()
    };
    first.configs = ImageConfigs::known([("tool@sha256:abcd".to_owned(), config)]);
    assert_eq!(run(&first, "run-K-1-1").await, "kept\n");
    let kept = root.path().join("scratch/images/sha256-abcd.config.json");
    assert!(kept.is_file(), "the config is kept next to the SIF file");
    // A later run knows no config, so only the kept file can give the default command.
    let mut later = backend(root.path(), &bin, container);
    later.configs = ImageConfigs::default();
    assert_eq!(run(&later, "run-K-2-1").await, "kept\n");
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
async fn containers_share_job() {
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
async fn continues_failed_dependencies() {
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
async fn docker_groups_images() {
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

#[tokio::test]
async fn stages_through_objects() {
    stage_objects(false, false).await;
}

#[tokio::test]
async fn stages_with_credentials() {
    stage_objects(true, false).await;
}

#[tokio::test]
async fn stages_uploaded_helper() {
    stage_objects(false, true).await;
}

/// Runs a task through a staging root, with presigned requests, or with the credentials of the
/// file of storage providers and a cleanup of the staging root. An `upload` job downloads the
/// helper from the staging root.
async fn stage_objects(credentials: bool, upload: bool) {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), NODE_APPTAINER);
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    // The job starts this test program as the helper, which runs `stage_helper`.
    let helper = root.path().join("iunctus");
    let program = std::env::current_exe().expect("this program");
    let script = format!(
        "#!/bin/sh\nIFS=$(printf '\\037')\nIUNCTUS_TEST_STAGE=\"$*\" exec {} --exact stage_helper \
         --nocapture --test-threads=1\n",
        text(&program)
    );
    std::fs::write(&helper, &script).expect("writes the helper");
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).expect("runs");
    let staging_root = root.path().join("staging");
    let objects = match credentials {
        false => content_store::Store::local(&staging_root).expect("a store"),
        true => {
            // The nodes reach the staging root as `s3://staging` through a folder provider.
            std::fs::create_dir_all(&staging_root).expect("makes the folder");
            let folder = opendal::services::Fs::default().root(&text(&staging_root));
            let operator = opendal::Operator::new(folder).expect("an operator");
            content_store::Store::remote(operator, "s3://staging").expect("a store")
        }
    };
    let secret_file = credentials.then(|| {
        let file = root.path().join("staging.toml");
        let providers = format!(
            "[storage.staging]\ntype = \"folder\"\npath = {:?}\nprefixes = [\"s3://staging/\"]\n",
            text(&staging_root)
        );
        std::fs::write(&file, providers).expect("writes the providers");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("private");
        file
    });
    backend.remote = Some(NodeStaging {
        stage: RemoteStage {
            objects,
            expiry: Duration::from_secs(600),
            max_outputs: 4,
            credentials,
            cleanup: credentials,
        },
        helper: match upload {
            false => Helper::Path(text(&helper)),
            true => Helper::Upload(Binary {
                digest: task_model::Digest::sha256(script.as_bytes()),
                file: helper.clone(),
                machine: match std::env::consts::ARCH {
                    "aarch64" => Machine::Arm64,
                    _ => Machine::Amd64,
                },
                uploaded: Default::default(),
            }),
        },
        folder: Some(text(&root.path().join("node"))),
        secret_file: secret_file.clone(),
    });
    let spec = spec("tr a-z A-Z < in.txt > out.txt && cat /opt/iunctus-test/extra.txt >> out.txt");
    let mut staging = staging(root.path(), "hello\n");
    let extra = root.path().join("source/extra.txt");
    std::fs::write(&extra, "extra\n").expect("writes the input");
    staging.inputs.push(Staged {
        path: "/opt/iunctus-test/extra.txt".into(),
        source: extra,
        ..Staged::default()
    });
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
    assert_eq!(output, "HELLO\nextra\n");
    // The node pulled nothing for this machine, and the job had no shared folder.
    assert!(!bin.join("state/pulls").exists());
    let options = std::fs::read_to_string(bin.join("state/101.options")).expect("the options");
    assert!(
        options.contains("--chdir=/tmp\n--output=/dev/null\n"),
        "{options}"
    );
    let node = std::fs::read_dir(root.path().join("node")).expect("the node folder");
    assert_eq!(node.count(), 0, "the job removes its folder");
    assert!(finished.root.join("control/slurm.log").is_file());
    let stage = &backend.remote.as_ref().expect("the staging").stage;
    let left = stage
        .objects
        .objects("attempts/")
        .await
        .expect("the attempts");
    assert_eq!(left.is_empty(), credentials, "{left:?}");
    let input = task_model::Digest::sha256(b"hello\n");
    assert!(stage.objects.has_content(&input).await.expect("the input"));
    if upload {
        let command = std::fs::read_to_string(bin.join("state/101.command")).expect("a script");
        let script = std::fs::read_to_string(command.trim()).expect("the script");
        assert!(script.contains("[ \"$(uname -m)\" = "), "{script}");
        let helper = task_model::Digest::sha256(&std::fs::read(&helper).expect("the helper"));
        assert!(
            stage
                .objects
                .has_content(&helper)
                .await
                .expect("the helper")
        );
    }
    if let Some(file) = secret_file {
        // The job script names the plan, but not the providers, which come in the environment.
        let command = std::fs::read_to_string(bin.join("state/101.command")).expect("a script");
        let script = std::fs::read_to_string(command.trim()).expect("the script");
        assert!(script.contains("'s3://staging/cas/"), "{script}");
        assert!(!script.contains("storage.staging"), "{script}");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("shared");
        let error = slurm_executor::secret_text(&file).unwrap_err();
        assert!(error.contains("chmod 600"), "{error}");
    }
}

#[tokio::test]
async fn reports_helper_failures() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = cluster(root.path(), NODE_APPTAINER);
    let mut backend = backend(
        root.path(),
        &bin,
        Container::Apptainer(text(&bin.join("apptainer"))),
    );
    let objects = content_store::Store::local(&root.path().join("staging")).expect("a store");
    backend.remote = Some(NodeStaging {
        stage: RemoteStage {
            objects,
            expiry: Duration::from_secs(600),
            max_outputs: 4,
            credentials: false,
            cleanup: true,
        },
        helper: Helper::Download {
            url: "file:///nonexistent/iunctus".into(),
            sha256: "ab".repeat(32),
        },
        folder: Some(text(&root.path().join("node"))),
        secret_file: None,
    });
    let spec = spec("echo never > out.txt");
    let (staging, placement) = (staging(root.path(), "hello\n"), placement());
    let job = Job {
        name: "run-P-13-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = (backend.run(job, &CancellationToken::new()).await).unwrap_err();
    // The job sends its log back without the helper, which tells why it failed.
    let expected = "cannot get the helper from file:///nonexistent/iunctus on ";
    assert!(error.to_string().contains(expected), "{error}");
    let node = std::fs::read_dir(root.path().join("node")).expect("the node folder");
    assert_eq!(node.count(), 0, "the job removes its folder");
    // The failed attempt leaves its input and wrapper, but no plan and no results.
    let stage = &backend.remote.as_ref().expect("the staging").stage;
    let left = (stage.objects.objects("attempts/").await).expect("the attempts");
    assert!(left.is_empty(), "{left:?}");
    let contents = (stage.objects.objects("cas/").await).expect("the contents");
    assert_eq!(contents.len(), 2, "{contents:?}");
}

/// Runs the staging helper when a job of `stage_objects` starts this program as it.
#[test]
fn stage_helper() {
    if let Ok(arguments) = std::env::var("IUNCTUS_TEST_STAGE") {
        let arguments: Vec<String> = arguments.split('\u{1f}').map(str::to_owned).collect();
        // The first argument is `__stage`, as the iunctus program takes it.
        executor::helper::main(&arguments[1..]).expect("the helper stages");
    }
}

/// Waits until `ready` holds, for at most 30 seconds, as a busy machine may be slow.
async fn until(ready: impl Fn() -> bool) {
    for _ in 0..3000 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the test waited too long");
}

/// Whether the journal of an attempt names the identity of its submitted job.
fn submitted(journal: &Path) -> bool {
    let record = std::fs::read(journal).ok();
    let record = record.and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    record.is_some_and(|record| record["identity"].is_object())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
