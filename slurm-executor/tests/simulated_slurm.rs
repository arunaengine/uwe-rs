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

use executor::{Backend, EngineCommand, ExecutorError, Job, Staged, Staging};
use slurm_executor::{Container, SlurmBackend};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
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

/// Runs the command after the image in the folder of `--pwd`, without a container.
const APPTAINER: &str = r#"#!/bin/bash
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
    backend
}

fn spec(script: &str) -> TaskSpec {
    TaskSpec {
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
    let options = std::fs::read_to_string(bin.join("state/101.options")).expect("the options");
    assert!(options.contains("--mem=256M\n--account=lab\n"), "{options}");
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

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
